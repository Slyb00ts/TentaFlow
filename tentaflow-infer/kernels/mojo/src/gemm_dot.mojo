# =============================================================================
# Plik: gemm_dot.mojo
# Opis: Prefillowy GEMM dla kart BEZ jednostki macierzowej — Y[T,rows] =
#       X[T,cols] · W^T na instrukcjach dot (`v_dot2_f32_f16` / `v_dot4_i32_i8`)
#       z akumulacją w rejestrach. Zastępuje ścieżkę mma/ldmatrix na RDNA2.
# Przykład: gemm_f16_dot2_128x128_t512 = gemm_f16_dot2_impl[128, 128, 8, 4]
# =============================================================================
#
# Dlaczego to jest osobny kernel, a nie odnoga `gemm.mojo`: tamta rodzina jest
# zbudowana wokół kontraktu fragmentów `mma.m16n8k16` i `ldmatrix`. Rozkład
# fragmentów rozrzuca elementy A/B po ścieżkach wektorowych tak, że wątek NIE ma
# danych na swój własny wynik — bez jednostki macierzowej trzeba by je zbierać
# instrukcjami cross-lane, co kosztuje więcej, niż daje. Zamiast emulować
# fragmenty, ten kernel zmienia dekompozycję: każdy wątek trzyma WŁASNY kafel
# wyjścia TM x TN w rejestrach i czyta swoje wiersze A oraz B wprost z LDS.
#
# Pomiary z tej karty dyktują wymiary (patrz docs/STATUS.md, roofline):
#  1. Potrzeba >= 8 niezależnych łańcuchów akumulacji, bo VALU RDNA2 ma
#     kilkutaktową latencję wyniku. Wątek trzyma TM*TN akumulatorów i wszystkie
#     są różne w obrębie jednej pary k, więc łańcuch zależności nie dławi VALU.
#  2. Kafel bloku decyduje o ruchu globalnym: FLOP/bajt = 4*BM*BN/(BM+BN), czyli
#     32 dla 64x64 (~12 TFLOPS przy 386 GB/s) i 64 dla 128x128 (~25 TFLOPS).
#  3. RDNA2 nie ma `cp.async` (global -> LDS bez rejestrów), więc podwójne
#     buforowanie jest ręczne: kafel następnego etapu jedzie do REJESTRÓW przed
#     policzeniem bieżącego, a do LDS trafia po barierze następnej iteracji.
#
# UKŁAD LDS: kafle trzymamy PARAMI k, z wierszami ciągłymi —
# `tile[k/2][row][k%2]`. To nie jest kosmetyka, a główny czynnik wydajności:
# przy układzie wiersz-major lane czyta 16 B ze skokiem TN*stride (u nas 320 B),
# czyli cała fala trafia w 8 z 32 banków LDS i odczyt jest 4x dłuższy niż musi.
# Przy parach k lane `tx` czyta TN*2 połówek pod adresem `tx*TN*4` bajtów, więc
# fala czyta jeden CIĄGŁY blok i LDS pracuje z pełną szerokością. Para k leży
# obok siebie, bo tego właśnie wymaga jeden `v_dot2_f32_f16`. Dodatkowo układ
# nie potrzebuje dopełnienia wiersza, co zwalnia LDS na drugi workgroup na WGP.

from std.gpu import block_idx, thread_idx
from std.gpu.sync import barrier
from std.memory import stack_allocation
from std.gpu.memory import AddressSpace
from src.arch_dot import dot2_f16, dot4_i8, f8e4m3_to_f32, nvfp4_codes8
from src.gemv2 import _q4k_scale_min

comptime BK = 32  # kolumny na etap
comptime KSTEP = 8  # połówki wczytywane z pamięci globalnej jednym odczytem


@always_inline
def _load_tile[
    ROWS: Int, NT: Int
](
    src: UnsafePointer[Float16, MutAnyOrigin],
    base_row: Int,
    k0: Int,
    n_cols: Int,
    row_limit: Int,
    lrow: Int,
    kc: Int,
    mut regs: InlineArray[
        SIMD[DType.float16, KSTEP],
        (ROWS + (NT // 4) - 1) // (NT // 4),
    ],
):
    """Wczytuje kafel ROWS x BK z pamięci globalnej do rejestrów (4 wątki/wiersz).

    Ogon po k jest zerowany, a nie zaciskany: X i W mnożą się wzajemnie, więc
    zero po jednej stronie wystarcza, żeby iloczyn nie wpłynął na wynik. Wiersze
    poza zakresem czytają ostatni legalny wiersz — ich wyniki i tak nie są
    zapisywane.
    """
    comptime ROWS_PER_PASS = NT // 4
    comptime PASSES = (ROWS + ROWS_PER_PASS - 1) // ROWS_PER_PASS

    comptime for pass_id in range(PASSES):
        # Gdy kafel jest węższy niż jedno przejście (np. BN=128 przy 768
        # wątkach), część wątków nie ma czego wnosić i tylko pomija zapis.
        if lrow + pass_id * ROWS_PER_PASS < ROWS:
            var global_row = base_row + lrow + pass_id * ROWS_PER_PASS
            if global_row > row_limit - 1:
                global_row = row_limit - 1
            if k0 + kc + KSTEP <= n_cols:
                regs[pass_id] = (src + global_row * n_cols + k0 + kc).load[
                    width=KSTEP
                ]()
            else:
                regs[pass_id] = SIMD[DType.float16, KSTEP](0.0)


@always_inline
def _store_tile[
    ROWS: Int, NT: Int
](
    dst: UnsafePointer[
        Float16, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    lrow: Int,
    kc: Int,
    regs: InlineArray[
        SIMD[DType.float16, KSTEP],
        (ROWS + (NT // 4) - 1) // (NT // 4),
    ],
):
    """Rozkłada wczytany kafel do LDS w układzie `tile[k/2][row][k%2]`.

    Osiem kolejnych k jednego wiersza rozpada się na cztery pary, każda do innej
    płaszczyzny kp — czyli cztery zapisy po 4 B zamiast jednego po 16 B. Ten
    koszt (8 zapisów na etap na wątek) zwraca się wielokrotnie w pętli liczącej,
    która czyta z LDS kilkadziesiąt razy więcej.
    """
    comptime ROWS_PER_PASS = NT // 4
    comptime PASSES = (ROWS + ROWS_PER_PASS - 1) // ROWS_PER_PASS

    comptime for pass_id in range(PASSES):
        row = lrow + pass_id * ROWS_PER_PASS
        if row < ROWS:
            comptime for pair in range(KSTEP // 2):
                kp = kc // 2 + pair
                (dst + kp * (ROWS * 2) + row * 2).store[width=2, alignment=4](
                    regs[pass_id].slice[2, offset = pair * 2]()
                )


def gemm_f16_dot2_impl[BM: Int, BN: Int, TM: Int, TN: Int, OUT: DType](
    y: UnsafePointer[Scalar[OUT], MutAnyOrigin],
    w: UnsafePointer[Float16, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """f16 GEMM na `v_dot2_f32_f16`: Y[t, r] = dot(w[r], x[t]).

    Siatka (ceil(rows/BN), ceil(T/BM)), blok (BM/TM)*(BN/TN) wątków.
    WYMAGA `n_cols % 8 == 0` (kafel wnoszony odczytami po 8 połówek); ogon po BK
    jest zerowany, więc `n_cols` nie musi być wielokrotnością BK. Liczba tokenów
    i wierszy dowolna. Akumulacja w f32.
    """
    comptime NT = (BM // TM) * (BN // TN)
    comptime TILE = BM * BK
    comptime WTILE = BN * BK
    comptime ROWS_TX = BN // TN
    comptime X_PASSES = (BM + (NT // 4) - 1) // (NT // 4)
    comptime W_PASSES = (BN + (NT // 4) - 1) // (NT // 4)

    tid = Int(thread_idx.x)
    row0 = Int(block_idx.x) * BN
    t0 = Int(block_idx.y) * BM

    xs = stack_allocation[
        2 * TILE, Float16, address_space = AddressSpace.SHARED
    ]()
    ws = stack_allocation[
        2 * WTILE, Float16, address_space = AddressSpace.SHARED
    ]()

    lrow = tid // 4
    kc = (tid % 4) * KSTEP
    tx = tid % ROWS_TX
    ty = tid // ROWS_TX

    var acc = InlineArray[Float32, TM * TN](fill=0.0)
    var xr = InlineArray[SIMD[DType.float16, KSTEP], X_PASSES](
        uninitialized=True
    )
    var wr = InlineArray[SIMD[DType.float16, KSTEP], W_PASSES](
        uninitialized=True
    )

    n_stages = (n_cols + BK - 1) // BK
    _load_tile[BM, NT](x, t0, 0, n_cols, n_tokens, lrow, kc, xr)
    _load_tile[BN, NT](w, row0, 0, n_cols, n_rows, lrow, kc, wr)

    var stage = 0
    while stage < n_stages:
        buf = stage % 2
        _store_tile[BM, NT](xs + buf * TILE, lrow, kc, xr)
        _store_tile[BN, NT](ws + buf * WTILE, lrow, kc, wr)
        barrier()

        # Odczyty globalne następnego etapu lecą PRZED matematyką bieżącego, więc
        # ich latencja chowa się pod pętlą dot. Do LDS wejdą po barierze na
        # początku następnej iteracji, do drugiego bufora.
        if stage + 1 < n_stages:
            k_next = (stage + 1) * BK
            _load_tile[BM, NT](x, t0, k_next, n_cols, n_tokens, lrow, kc, xr)
            _load_tile[BN, NT](w, row0, k_next, n_cols, n_rows, lrow, kc, wr)

        xbase = xs + buf * TILE + ty * TM * 2
        wbase = ws + buf * WTILE + tx * TN * 2

        comptime for kp in range(BK // 2):
            av = (xbase + kp * (BM * 2)).load[width = TM * 2, alignment=16]()
            bv = (wbase + kp * (BN * 2)).load[width = TN * 2, alignment=16]()

            comptime for m in range(TM):
                comptime for r in range(TN):
                    acc[m * TN + r] = dot2_f16(
                        av.slice[2, offset = m * 2](),
                        bv.slice[2, offset = r * 2](),
                        acc[m * TN + r],
                    )
        barrier()
        stage += 1

    comptime for m in range(TM):
        t = t0 + ty * TM + m
        if t < n_tokens:
            comptime for r in range(TN):
                row = row0 + tx * TN + r
                if row < n_rows:
                    y[t * n_rows + row] = acc[m * TN + r].cast[OUT]()


# Instancje wybrane pomiarem na gfx1030 (4096x4096, T=1024): 256x64 i 128x128
# dają 23 TFLOPS, 128x64 22, a 64x64 16 — ten ostatni jest jednak potrzebny dla
# małych kształtów, gdzie duży kafel to w większości odrzucone obliczenia.
comptime gemm_f16_dot2_64x64 = gemm_f16_dot2_impl[64, 64, 4, 4, DType.float16]
comptime gemm_f16_dot2_128x64 = gemm_f16_dot2_impl[128, 64, 8, 4, DType.float16]
comptime gemm_f16_dot2_128x128 = gemm_f16_dot2_impl[
    128, 128, 8, 8, DType.float16
]
comptime gemm_f16_dot2_256x64 = gemm_f16_dot2_impl[256, 64, 8, 8, DType.float16]
# Batchowa głowa logitów zapisuje f32, żeby nie tracić dokładności akumulatora
# przed samplingiem — tak samo jak rodzina `gemm_*_out_f32` na NVIDII. Liczba
# tokenów to tam rozmiar batcha decode (<= 64), więc wystarczy najmniejszy kafel.
comptime gemm_f16_dot2_out_f32_64x64 = gemm_f16_dot2_impl[
    64, 64, 4, 4, DType.float32
]


@always_inline
def _q8_0_row_block(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    row: Int,
    blocks_per_row: Int,
    blk: Int,
) -> UnsafePointer[UInt8, MutAnyOrigin]:
    """Adres bloku `block_q8_0` (34 B: skala f16 + 32 kody int8) danego wiersza."""
    return w + (row * blocks_per_row + blk) * 34


def gemm_q8_0_dot4_impl[BM: Int, BN: Int, TM: Int, TN: Int, KB: Int, OUT: DType](
    y: UnsafePointer[Scalar[OUT], MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    xq: UnsafePointer[Int8, MutAnyOrigin],
    xd: UnsafePointer[Float32, MutAnyOrigin],
    xsm: UnsafePointer[Float32, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """Q8_0 GEMM na `v_dot4_i32_i8`: Y[t, r] = dot(w[r], x[t]).

    Aktywacja jest wcześniej skwantowana przez `quantize_act_q8_1` (kody int8
    `xq` w układzie [T, K], skale `xd` w układzie blok-major [K/32, T]); `xsm`
    jest w sygnaturze dla zgodności z rodziną i8mma i nie jest tu potrzebne, bo
    Q8_0 jest symetryczne. Wagi czytamy WPROST z bajtów GGUF, bez przepakowania.

    Iloczyny sumują się w int32 w obrębie 32-kolumnowego bloku kwantyzacji i
    dopiero potem są skalowane do f32 — dokładnie jak w MMQ. `KB` mówi, ile
    bloków wchodzi do LDS na jeden etap: bariera przypada raz na KB*32 kolumn,
    więc większe KB rozcieńcza koszt synchronizacji.

    Siatka (ceil(rows/BN), ceil(T/BM)), blok (BM/TM)*(BN/TN) wątków,
    `n_cols % 32 == 0` (niezmiennik formatu Q8_0).
    """
    comptime NT = (BM // TM) * (BN // TN)
    comptime ROWS_TX = BN // TN
    comptime X_PASSES = (BM + (NT // 4) - 1) // (NT // 4)
    comptime W_PASSES = (BN + (NT // 4) - 1) // (NT // 4)
    comptime XPLANE = BM * 32  # bajty jednego bloku: 8 czwórek k x BM x 4
    comptime WPLANE = BN * 32

    tid = Int(thread_idx.x)
    row0 = Int(block_idx.x) * BN
    t0 = Int(block_idx.y) * BM
    blocks_per_row = n_cols // 32

    xs = stack_allocation[
        KB * XPLANE, Int8, address_space = AddressSpace.SHARED
    ]()
    ws = stack_allocation[
        KB * WPLANE, Int8, address_space = AddressSpace.SHARED
    ]()
    xds = stack_allocation[
        KB * BM, Float32, address_space = AddressSpace.SHARED
    ]()
    wds = stack_allocation[
        KB * BN, Float32, address_space = AddressSpace.SHARED
    ]()

    lrow = tid // 4
    kc = (tid % 4) * 8  # osiem kodów na wątek
    tx = tid % ROWS_TX
    ty = tid // ROWS_TX

    var acc = InlineArray[Float32, TM * TN](fill=0.0)

    var base_blk = 0
    while base_blk < blocks_per_row:
        comptime for kb in range(KB):
            blk = base_blk + kb
            live = blk < blocks_per_row

            comptime for p in range(X_PASSES):
                local = lrow + p * (NT // 4)
                if local < BM:
                    var token = t0 + local
                    if token > n_tokens - 1:
                        token = n_tokens - 1
                    var bytes8 = SIMD[DType.int8, 8](0)
                    if live:
                        bytes8 = (
                            xq + token * n_cols + blk * 32 + kc
                        ).load[width=8]()
                    comptime for q in range(2):
                        kq = kc // 4 + q
                        (
                            xs + kb * XPLANE + kq * (BM * 4) + local * 4
                        ).store[width=4, alignment=4](
                            bytes8.slice[4, offset = q * 4]()
                        )

            comptime for p in range(W_PASSES):
                local = lrow + p * (NT // 4)
                if local < BN:
                    var row = row0 + local
                    if row > n_rows - 1:
                        row = n_rows - 1
                    var codes = SIMD[DType.int8, 8](0)
                    var scale: Float32 = 0.0
                    if live:
                        block_ptr = _q8_0_row_block(
                            w, row, blocks_per_row, blk
                        )
                        codes = (block_ptr + 2 + kc).bitcast[Int8]().load[
                            width=8
                        ]()
                        scale = Float32(block_ptr.bitcast[Float16]().load())
                    comptime for q in range(2):
                        kq = kc // 4 + q
                        (
                            ws + kb * WPLANE + kq * (BN * 4) + local * 4
                        ).store[width=4, alignment=4](
                            codes.slice[4, offset = q * 4]()
                        )
                    if tid % 4 == 0:
                        wds[kb * BN + local] = scale

            if tid < BM:
                var token = t0 + tid
                if token > n_tokens - 1:
                    token = n_tokens - 1
                xds[kb * BM + tid] = xd[blk * n_tokens + token] if live else 0.0
        barrier()

        comptime for kb in range(KB):
            xbase = xs + kb * XPLANE + ty * TM * 4
            wbase = ws + kb * WPLANE + tx * TN * 4
            var isum = InlineArray[Int32, TM * TN](fill=0)

            comptime for kq in range(8):
                av = (xbase + kq * (BM * 4)).bitcast[Int32]().load[
                    width=TM, alignment=16
                ]()
                bv = (wbase + kq * (BN * 4)).bitcast[Int32]().load[
                    width=TN, alignment=16
                ]()

                comptime for m in range(TM):
                    comptime for r in range(TN):
                        isum[m * TN + r] = dot4_i8(
                            av[m], bv[r], isum[m * TN + r]
                        )

            comptime for m in range(TM):
                dx = xds[kb * BM + ty * TM + m]
                comptime for r in range(TN):
                    acc[m * TN + r] += (
                        dx
                        * wds[kb * BN + tx * TN + r]
                        * Float32(isum[m * TN + r])
                    )
        barrier()
        base_blk += KB

    comptime for m in range(TM):
        t = t0 + ty * TM + m
        if t < n_tokens:
            comptime for r in range(TN):
                row = row0 + tx * TN + r
                if row < n_rows:
                    y[t * n_rows + row] = acc[m * TN + r].cast[OUT]()


# KB dobrane pomiarem: dla kafla 128x128 dwa bloki na etap dają 35 TOPS wobec
# 33 przy jednym, a cztery spadają do 14, bo LDS przestaje mieścić dwa
# workgroupy na WGP. Mały kafel 64x64 ma zapas LDS i znosi KB=4.
comptime gemm_q8_0_dot4_64x64 = gemm_q8_0_dot4_impl[
    64, 64, 4, 4, 4, DType.float16
]
comptime gemm_q8_0_dot4_128x64 = gemm_q8_0_dot4_impl[
    128, 64, 8, 4, 2, DType.float16
]
comptime gemm_q8_0_dot4_128x128 = gemm_q8_0_dot4_impl[
    128, 128, 8, 4, 2, DType.float16
]
comptime gemm_q8_0_dot4_out_f32_64x64 = gemm_q8_0_dot4_impl[
    64, 64, 4, 4, 4, DType.float32
]


def gemm_q4_k_dot4_impl[BM: Int, BN: Int, TM: Int, TN: Int, KB: Int, OUT: DType](
    y: UnsafePointer[Scalar[OUT], MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    xq: UnsafePointer[Int8, MutAnyOrigin],
    xd: UnsafePointer[Float32, MutAnyOrigin],
    xsm: UnsafePointer[Float32, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """Q4_K GEMM na `v_dot4_i32_i8`: Y[t, r] = dot(w[r], x[t]).

    Wagi czytamy wprost z 144-bajtowych superbloków GGUF (16 B nagłówka: d, dmin
    i dwanaście 6-bitowych par skal, potem 128 B nibbli na 256 kolumn). Q4_K jest
    ASYMETRYCZNE: `w = d*sc*q - dmin*m`, więc wkład 32-kolumnowego podbloku to
    `xd*d*sc*<q, xq> - dmin*m*xsm`, gdzie `xsm` (z `quantize_act_q8_1`) już niesie
    `xd * suma(xq)`. Ta druga składowa jest jedyną różnicą wobec ścieżki Q8_0.

    `n_cols % 256 == 0` (niezmiennik formatu), siatka i blok jak w Q8_0.
    """
    comptime NT = (BM // TM) * (BN // TN)
    comptime ROWS_TX = BN // TN
    comptime X_PASSES = (BM + (NT // 4) - 1) // (NT // 4)
    comptime W_PASSES = (BN + (NT // 4) - 1) // (NT // 4)
    comptime XPLANE = BM * 32
    comptime WPLANE = BN * 32

    tid = Int(thread_idx.x)
    row0 = Int(block_idx.x) * BN
    t0 = Int(block_idx.y) * BM
    supers_per_row = n_cols // 256
    total_sub = n_cols // 32

    xs = stack_allocation[
        KB * XPLANE, Int8, address_space = AddressSpace.SHARED
    ]()
    ws = stack_allocation[
        KB * WPLANE, Int8, address_space = AddressSpace.SHARED
    ]()
    xds = stack_allocation[
        KB * BM, Float32, address_space = AddressSpace.SHARED
    ]()
    xsms = stack_allocation[
        KB * BM, Float32, address_space = AddressSpace.SHARED
    ]()
    wds = stack_allocation[
        KB * BN, Float32, address_space = AddressSpace.SHARED
    ]()
    wdm = stack_allocation[
        KB * BN, Float32, address_space = AddressSpace.SHARED
    ]()

    lrow = tid // 4
    kc = (tid % 4) * 8
    tx = tid % ROWS_TX
    ty = tid // ROWS_TX

    var acc = InlineArray[Float32, TM * TN](fill=0.0)

    var base_sub = 0
    while base_sub < total_sub:
        comptime for kb in range(KB):
            js = base_sub + kb
            live = js < total_sub

            comptime for p in range(X_PASSES):
                local = lrow + p * (NT // 4)
                if local < BM:
                    var token = t0 + local
                    if token > n_tokens - 1:
                        token = n_tokens - 1
                    var bytes8 = SIMD[DType.int8, 8](0)
                    if live:
                        bytes8 = (xq + token * n_cols + js * 32 + kc).load[
                            width=8
                        ]()
                    comptime for q in range(2):
                        kq = kc // 4 + q
                        (
                            xs + kb * XPLANE + kq * (BM * 4) + local * 4
                        ).store[width=4, alignment=4](
                            bytes8.slice[4, offset = q * 4]()
                        )

            comptime for p in range(W_PASSES):
                local = lrow + p * (NT // 4)
                if local < BN:
                    var row = row0 + local
                    if row > n_rows - 1:
                        row = n_rows - 1
                    var codes = SIMD[DType.int8, 8](0)
                    var dsc: Float32 = 0.0
                    var dmm: Float32 = 0.0
                    if live:
                        sb = js // 8
                        j = js % 8
                        block_ptr = w + (row * supers_per_row + sb) * 144
                        header = block_ptr.load[width=16, alignment=4]()
                        d = Float32(
                            block_ptr.bitcast[Float16]().load[width=1]()
                        )
                        dmin = Float32(
                            (block_ptr + 2).bitcast[Float16]().load[width=1]()
                        )
                        sc, mn = _q4k_scale_min(header, j)
                        dsc = d * sc
                        dmm = dmin * mn
                        # Nibble niski to podblok parzysty, wysoki to nieparzysty;
                        # oba dzielą te same 32 bajty `qs`.
                        packed = (block_ptr + 16 + 32 * (j // 2) + kc).load[
                            width=8
                        ]()
                        if j % 2 == 0:
                            codes = (packed & 0x0F).cast[DType.int8]()
                        else:
                            codes = (packed >> 4).cast[DType.int8]()
                    comptime for q in range(2):
                        kq = kc // 4 + q
                        (
                            ws + kb * WPLANE + kq * (BN * 4) + local * 4
                        ).store[width=4, alignment=4](
                            codes.slice[4, offset = q * 4]()
                        )
                    if tid % 4 == 0:
                        wds[kb * BN + local] = dsc
                        wdm[kb * BN + local] = dmm

            if tid < BM:
                var token = t0 + tid
                if token > n_tokens - 1:
                    token = n_tokens - 1
                xds[kb * BM + tid] = xd[js * n_tokens + token] if live else 0.0
                xsms[kb * BM + tid] = xsm[
                    js * n_tokens + token
                ] if live else 0.0
        barrier()

        comptime for kb in range(KB):
            xbase = xs + kb * XPLANE + ty * TM * 4
            wbase = ws + kb * WPLANE + tx * TN * 4
            var isum = InlineArray[Int32, TM * TN](fill=0)

            comptime for kq in range(8):
                av = (xbase + kq * (BM * 4)).bitcast[Int32]().load[
                    width=TM, alignment=16
                ]()
                bv = (wbase + kq * (BN * 4)).bitcast[Int32]().load[
                    width=TN, alignment=16
                ]()

                comptime for m in range(TM):
                    comptime for r in range(TN):
                        isum[m * TN + r] = dot4_i8(
                            av[m], bv[r], isum[m * TN + r]
                        )

            comptime for m in range(TM):
                dx = xds[kb * BM + ty * TM + m]
                sx = xsms[kb * BM + ty * TM + m]
                comptime for r in range(TN):
                    acc[m * TN + r] += (
                        dx * wds[kb * BN + tx * TN + r] * Float32(
                            isum[m * TN + r]
                        )
                        - sx * wdm[kb * BN + tx * TN + r]
                    )
        barrier()
        base_sub += KB

    comptime for m in range(TM):
        t = t0 + ty * TM + m
        if t < n_tokens:
            comptime for r in range(TN):
                row = row0 + tx * TN + r
                if row < n_rows:
                    y[t * n_rows + row] = acc[m * TN + r].cast[OUT]()


comptime gemm_q4_k_dot4_64x64 = gemm_q4_k_dot4_impl[
    64, 64, 4, 4, 4, DType.float16
]
comptime gemm_q4_k_dot4_128x64 = gemm_q4_k_dot4_impl[
    128, 64, 8, 4, 2, DType.float16
]
comptime gemm_q4_k_dot4_128x128 = gemm_q4_k_dot4_impl[
    128, 128, 8, 4, 2, DType.float16
]
comptime gemm_q4_k_dot4_out_f32_64x64 = gemm_q4_k_dot4_impl[
    64, 64, 4, 4, 4, DType.float32
]


def gemm_q6_k_dot4_impl[BM: Int, BN: Int, TM: Int, TN: Int, KB: Int, OUT: DType](
    y: UnsafePointer[Scalar[OUT], MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    xq: UnsafePointer[Int8, MutAnyOrigin],
    xd: UnsafePointer[Float32, MutAnyOrigin],
    xsm: UnsafePointer[Float32, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """Q6_K GEMM na `v_dot4_i32_i8`: Y[t, r] = dot(w[r], x[t]).

    Superblok GGUF ma 210 B na 256 kolumn: 128 B młodszych nibbli, 64 B po dwa
    starsze bity, 16 skal int8 i wspólne `d`. Wartość to `d * sc * (q - 32)`.
    Przesunięcie -32 stosujemy JUŻ PRZY ZAPISIE do LDS (zakres -32..31 mieści
    się w int8), więc iloczyn skalarny nie potrzebuje członu z sumą aktywacji i
    `xsm` jest tu nieużywane — inaczej niż w Q4_K.

    Jedna skala przypada na 16 kolumn, a blok kwantyzacji aktywacji ma 32, więc
    32-kolumnowy podblok akumuluje DWIE niezależne sumy int32 i każda dostaje
    własną skalę. `n_cols % 256 == 0`.
    """
    comptime NT = (BM // TM) * (BN // TN)
    comptime ROWS_TX = BN // TN
    comptime X_PASSES = (BM + (NT // 4) - 1) // (NT // 4)
    comptime W_PASSES = (BN + (NT // 4) - 1) // (NT // 4)
    comptime XPLANE = BM * 32
    comptime WPLANE = BN * 32

    tid = Int(thread_idx.x)
    row0 = Int(block_idx.x) * BN
    t0 = Int(block_idx.y) * BM
    supers_per_row = n_cols // 256
    total_sub = n_cols // 32

    xs = stack_allocation[
        KB * XPLANE, Int8, address_space = AddressSpace.SHARED
    ]()
    ws = stack_allocation[
        KB * WPLANE, Int8, address_space = AddressSpace.SHARED
    ]()
    xds = stack_allocation[
        KB * BM, Float32, address_space = AddressSpace.SHARED
    ]()
    wlo = stack_allocation[
        KB * BN, Float32, address_space = AddressSpace.SHARED
    ]()
    whi = stack_allocation[
        KB * BN, Float32, address_space = AddressSpace.SHARED
    ]()

    lrow = tid // 4
    kc = (tid % 4) * 8
    tx = tid % ROWS_TX
    ty = tid // ROWS_TX

    var acc = InlineArray[Float32, TM * TN](fill=0.0)

    var base_sub = 0
    while base_sub < total_sub:
        comptime for kb in range(KB):
            js = base_sub + kb
            live = js < total_sub

            comptime for p in range(X_PASSES):
                local = lrow + p * (NT // 4)
                if local < BM:
                    var token = t0 + local
                    if token > n_tokens - 1:
                        token = n_tokens - 1
                    var bytes8 = SIMD[DType.int8, 8](0)
                    if live:
                        bytes8 = (xq + token * n_cols + js * 32 + kc).load[
                            width=8
                        ]()
                    comptime for q in range(2):
                        kq = kc // 4 + q
                        (
                            xs + kb * XPLANE + kq * (BM * 4) + local * 4
                        ).store[width=4, alignment=4](
                            bytes8.slice[4, offset = q * 4]()
                        )

            comptime for p in range(W_PASSES):
                local = lrow + p * (NT // 4)
                if local < BN:
                    var row = row0 + local
                    if row > n_rows - 1:
                        row = n_rows - 1
                    var codes = SIMD[DType.int8, 8](0)
                    var scale: Float32 = 0.0
                    if live:
                        sb = js // 8
                        c0 = (js % 8) * 32 + kc
                        half = c0 // 128
                        g = (c0 % 128) // 32
                        l = c0 % 32
                        block_ptr = w + (row * supers_per_row + sb) * 210
                        d = Float32(
                            (block_ptr + 208).bitcast[Float16]().load[width=1]()
                        )
                        sc = Int32(
                            (block_ptr + 192 + half * 8 + l // 16 + 2 * g)
                            .bitcast[Int8]()
                            .load[width=1]()
                        )
                        scale = d * Float32(sc)
                        low = (
                            block_ptr + half * 64 + l + (g % 2) * 32
                        ).load[width=8]()
                        high = (block_ptr + 128 + half * 32 + l).load[width=8]()
                        nib = (
                            low >> SIMD[DType.uint8, 8]((g // 2) * 4)
                        ) & 0x0F
                        top = (high >> SIMD[DType.uint8, 8](2 * g)) & 0x03
                        codes = (nib | (top << 4)).cast[DType.int8]() - 32
                    comptime for q in range(2):
                        kq = kc // 4 + q
                        (
                            ws + kb * WPLANE + kq * (BN * 4) + local * 4
                        ).store[width=4, alignment=4](
                            codes.slice[4, offset = q * 4]()
                        )
                    if kc == 0:
                        wlo[kb * BN + local] = scale
                    elif kc == 16:
                        whi[kb * BN + local] = scale

            if tid < BM:
                var token = t0 + tid
                if token > n_tokens - 1:
                    token = n_tokens - 1
                xds[kb * BM + tid] = xd[js * n_tokens + token] if live else 0.0
        barrier()

        comptime for kb in range(KB):
            xbase = xs + kb * XPLANE + ty * TM * 4
            wbase = ws + kb * WPLANE + tx * TN * 4
            var slo = InlineArray[Int32, TM * TN](fill=0)
            var shi = InlineArray[Int32, TM * TN](fill=0)

            comptime for kq in range(8):
                av = (xbase + kq * (BM * 4)).bitcast[Int32]().load[
                    width=TM, alignment=16
                ]()
                bv = (wbase + kq * (BN * 4)).bitcast[Int32]().load[
                    width=TN, alignment=16
                ]()

                comptime for m in range(TM):
                    comptime for r in range(TN):
                        comptime if kq < 4:
                            slo[m * TN + r] = dot4_i8(
                                av[m], bv[r], slo[m * TN + r]
                            )
                        else:
                            shi[m * TN + r] = dot4_i8(
                                av[m], bv[r], shi[m * TN + r]
                            )

            comptime for m in range(TM):
                dx = xds[kb * BM + ty * TM + m]
                comptime for r in range(TN):
                    acc[m * TN + r] += dx * (
                        wlo[kb * BN + tx * TN + r] * Float32(slo[m * TN + r])
                        + whi[kb * BN + tx * TN + r] * Float32(shi[m * TN + r])
                    )
        barrier()
        base_sub += KB

    comptime for m in range(TM):
        t = t0 + ty * TM + m
        if t < n_tokens:
            comptime for r in range(TN):
                row = row0 + tx * TN + r
                if row < n_rows:
                    y[t * n_rows + row] = acc[m * TN + r].cast[OUT]()


comptime gemm_q6_k_dot4_64x64 = gemm_q6_k_dot4_impl[
    64, 64, 4, 4, 4, DType.float16
]
comptime gemm_q6_k_dot4_128x64 = gemm_q6_k_dot4_impl[
    128, 64, 8, 4, 2, DType.float16
]
comptime gemm_q6_k_dot4_out_f32_64x64 = gemm_q6_k_dot4_impl[
    64, 64, 4, 4, 4, DType.float32
]


def gemm_nvfp4_dot4_impl[BM: Int, BN: Int, TM: Int, TN: Int, KB: Int, OUT: DType](
    y: UnsafePointer[Scalar[OUT], MutAnyOrigin],
    packed: UnsafePointer[UInt8, MutAnyOrigin],
    scales: UnsafePointer[UInt8, MutAnyOrigin],
    xq: UnsafePointer[Int8, MutAnyOrigin],
    xd: UnsafePointer[Float32, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
    inv_global_scale: Float32,
):
    """NVFP4 GEMM na `v_dot4_i32_i8`: Y[t, r] = dot(w[r], x[t]).

    Wagi to spakowane e2m1 (dwie kolumny na bajt, młodszy półbajt to kolumna
    parzysta) plus jedna skala `float8_e4m3` na 16 kolumn i wspólna skala
    tensora. Kody rozpakowujemy PODWOJONE (patrz `nvfp4_codes8`), przez co
    iloczyn skalarny jest dokładny w int32, a czynnik 2 pochłania skala grupy.

    Blok kwantyzacji aktywacji ma 32 kolumny, a grupa wag 16, więc 32-kolumnowy
    podblok akumuluje DWIE niezależne sumy int32 — tak samo jak Q6_K.
    `n_cols % 32 == 0`.
    """
    comptime NT = (BM // TM) * (BN // TN)
    comptime ROWS_TX = BN // TN
    comptime X_PASSES = (BM + (NT // 4) - 1) // (NT // 4)
    comptime W_PASSES = (BN + (NT // 4) - 1) // (NT // 4)
    comptime XPLANE = BM * 32
    comptime WPLANE = BN * 32

    tid = Int(thread_idx.x)
    row0 = Int(block_idx.x) * BN
    t0 = Int(block_idx.y) * BM
    total_sub = n_cols // 32
    groups_per_row = n_cols // 16

    xs = stack_allocation[
        KB * XPLANE, Int8, address_space = AddressSpace.SHARED
    ]()
    ws = stack_allocation[
        KB * WPLANE, Int8, address_space = AddressSpace.SHARED
    ]()
    xds = stack_allocation[
        KB * BM, Float32, address_space = AddressSpace.SHARED
    ]()
    wlo = stack_allocation[
        KB * BN, Float32, address_space = AddressSpace.SHARED
    ]()
    whi = stack_allocation[
        KB * BN, Float32, address_space = AddressSpace.SHARED
    ]()

    lrow = tid // 4
    kc = (tid % 4) * 8
    tx = tid % ROWS_TX
    ty = tid // ROWS_TX

    var acc = InlineArray[Float32, TM * TN](fill=0.0)

    var base_sub = 0
    while base_sub < total_sub:
        comptime for kb in range(KB):
            js = base_sub + kb
            live = js < total_sub

            comptime for p in range(X_PASSES):
                local = lrow + p * (NT // 4)
                if local < BM:
                    var token = t0 + local
                    if token > n_tokens - 1:
                        token = n_tokens - 1
                    var bytes8 = SIMD[DType.int8, 8](0)
                    if live:
                        bytes8 = (xq + token * n_cols + js * 32 + kc).load[
                            width=8
                        ]()
                    comptime for q in range(2):
                        kq = kc // 4 + q
                        (
                            xs + kb * XPLANE + kq * (BM * 4) + local * 4
                        ).store[width=4, alignment=4](
                            bytes8.slice[4, offset = q * 4]()
                        )

            comptime for p in range(W_PASSES):
                local = lrow + p * (NT // 4)
                if local < BN:
                    var row = row0 + local
                    if row > n_rows - 1:
                        row = n_rows - 1
                    var codes = SIMD[DType.int8, 8](0)
                    var scale: Float32 = 0.0
                    if live:
                        nibbles = (
                            packed + row * (n_cols // 2) + js * 16 + kc // 2
                        ).load[width=4, alignment=4]()
                        codes = nvfp4_codes8(nibbles)
                        # 0,5 kompensuje podwojenie kodów w `nvfp4_codes8`.
                        scale = (
                            f8e4m3_to_f32(
                                scales[
                                    row * groups_per_row + js * 2 + kc // 16
                                ]
                            )
                            * inv_global_scale
                            * 0.5
                        )
                    comptime for q in range(2):
                        kq = kc // 4 + q
                        (
                            ws + kb * WPLANE + kq * (BN * 4) + local * 4
                        ).store[width=4, alignment=4](
                            codes.slice[4, offset = q * 4]()
                        )
                    if kc == 0:
                        wlo[kb * BN + local] = scale
                    elif kc == 16:
                        whi[kb * BN + local] = scale

            if tid < BM:
                var token = t0 + tid
                if token > n_tokens - 1:
                    token = n_tokens - 1
                xds[kb * BM + tid] = xd[js * n_tokens + token] if live else 0.0
        barrier()

        comptime for kb in range(KB):
            xbase = xs + kb * XPLANE + ty * TM * 4
            wbase = ws + kb * WPLANE + tx * TN * 4
            var slo = InlineArray[Int32, TM * TN](fill=0)
            var shi = InlineArray[Int32, TM * TN](fill=0)

            comptime for kq in range(8):
                av = (xbase + kq * (BM * 4)).bitcast[Int32]().load[
                    width=TM, alignment=16
                ]()
                bv = (wbase + kq * (BN * 4)).bitcast[Int32]().load[
                    width=TN, alignment=16
                ]()

                comptime for m in range(TM):
                    comptime for r in range(TN):
                        comptime if kq < 4:
                            slo[m * TN + r] = dot4_i8(
                                av[m], bv[r], slo[m * TN + r]
                            )
                        else:
                            shi[m * TN + r] = dot4_i8(
                                av[m], bv[r], shi[m * TN + r]
                            )

            comptime for m in range(TM):
                dx = xds[kb * BM + ty * TM + m]
                comptime for r in range(TN):
                    acc[m * TN + r] += dx * (
                        wlo[kb * BN + tx * TN + r] * Float32(slo[m * TN + r])
                        + whi[kb * BN + tx * TN + r] * Float32(shi[m * TN + r])
                    )
        barrier()
        base_sub += KB

    comptime for m in range(TM):
        t = t0 + ty * TM + m
        if t < n_tokens:
            comptime for r in range(TN):
                row = row0 + tx * TN + r
                if row < n_rows:
                    y[t * n_rows + row] = acc[m * TN + r].cast[OUT]()


comptime gemm_nvfp4_dot4_64x64 = gemm_nvfp4_dot4_impl[
    64, 64, 4, 4, 4, DType.float16
]
comptime gemm_nvfp4_dot4_128x64 = gemm_nvfp4_dot4_impl[
    128, 64, 8, 4, 2, DType.float16
]


def gemm_q4_0_dot4_impl[BM: Int, BN: Int, TM: Int, TN: Int, KB: Int, OUT: DType](
    y: UnsafePointer[Scalar[OUT], MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    xq: UnsafePointer[Int8, MutAnyOrigin],
    xd: UnsafePointer[Float32, MutAnyOrigin],
    xsm: UnsafePointer[Float32, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """Q4_0 GEMM na `v_dot4_i32_i8`: Y[t, r] = dot(w[r], x[t]).

    Blok GGUF ma 18 B na 32 wartości: skala f16 i 16 bajtów półbajtów, przy czym
    bajt `j` niesie wartość `j` w młodszym półbajcie i `j+16` w starszym (podział
    na połowy, nie przeplot). Wartość to `d * (q - 8)`, więc przesunięcie -8
    stosujemy PRZY ZAPISIE do LDS — zakres -8..7 mieści się w int8, a iloczyn
    skalarny nie potrzebuje członu z sumą aktywacji.

    Blok kwantyzacji wagi i aktywacji ma tu ten sam rozmiar 32 kolumn, więc na
    podblok wystarcza JEDNA suma int32 — inaczej niż w Q4_K i Q6_K. `xsm` jest w
    sygnaturze dla zgodności z rodziną i8mma i nie jest używane.
    """
    comptime NT = (BM // TM) * (BN // TN)
    comptime ROWS_TX = BN // TN
    comptime X_PASSES = (BM + (NT // 4) - 1) // (NT // 4)
    comptime W_PASSES = (BN + (NT // 4) - 1) // (NT // 4)
    comptime XPLANE = BM * 32
    comptime WPLANE = BN * 32

    tid = Int(thread_idx.x)
    row0 = Int(block_idx.x) * BN
    t0 = Int(block_idx.y) * BM
    blocks_per_row = n_cols // 32

    xs = stack_allocation[
        KB * XPLANE, Int8, address_space = AddressSpace.SHARED
    ]()
    ws = stack_allocation[
        KB * WPLANE, Int8, address_space = AddressSpace.SHARED
    ]()
    xds = stack_allocation[
        KB * BM, Float32, address_space = AddressSpace.SHARED
    ]()
    wds = stack_allocation[
        KB * BN, Float32, address_space = AddressSpace.SHARED
    ]()

    lrow = tid // 4
    kc = (tid % 4) * 8
    tx = tid % ROWS_TX
    ty = tid // ROWS_TX

    var acc = InlineArray[Float32, TM * TN](fill=0.0)

    var base_blk = 0
    while base_blk < blocks_per_row:
        comptime for kb in range(KB):
            blk = base_blk + kb
            live = blk < blocks_per_row

            comptime for p in range(X_PASSES):
                local = lrow + p * (NT // 4)
                if local < BM:
                    var token = t0 + local
                    if token > n_tokens - 1:
                        token = n_tokens - 1
                    var bytes8 = SIMD[DType.int8, 8](0)
                    if live:
                        bytes8 = (xq + token * n_cols + blk * 32 + kc).load[
                            width=8
                        ]()
                    comptime for q in range(2):
                        kq = kc // 4 + q
                        (
                            xs + kb * XPLANE + kq * (BM * 4) + local * 4
                        ).store[width=4, alignment=4](
                            bytes8.slice[4, offset = q * 4]()
                        )

            comptime for p in range(W_PASSES):
                local = lrow + p * (NT // 4)
                if local < BN:
                    var row = row0 + local
                    if row > n_rows - 1:
                        row = n_rows - 1
                    var codes = SIMD[DType.int8, 8](0)
                    var scale: Float32 = 0.0
                    if live:
                        block_ptr = w + (row * blocks_per_row + blk) * 18
                        scale = Float32(
                            block_ptr.bitcast[Float16]().load[width=1]()
                        )
                        packed = (block_ptr + 2 + kc % 16).load[width=8]()
                        nib = (
                            packed >> 4
                        ) if kc >= 16 else (packed & 0x0F)
                        codes = nib.cast[DType.int8]() - 8
                    comptime for q in range(2):
                        kq = kc // 4 + q
                        (
                            ws + kb * WPLANE + kq * (BN * 4) + local * 4
                        ).store[width=4, alignment=4](
                            codes.slice[4, offset = q * 4]()
                        )
                    if tid % 4 == 0:
                        wds[kb * BN + local] = scale

            if tid < BM:
                var token = t0 + tid
                if token > n_tokens - 1:
                    token = n_tokens - 1
                xds[kb * BM + tid] = xd[blk * n_tokens + token] if live else 0.0
        barrier()

        comptime for kb in range(KB):
            xbase = xs + kb * XPLANE + ty * TM * 4
            wbase = ws + kb * WPLANE + tx * TN * 4
            var isum = InlineArray[Int32, TM * TN](fill=0)

            comptime for kq in range(8):
                av = (xbase + kq * (BM * 4)).bitcast[Int32]().load[
                    width=TM, alignment=16
                ]()
                bv = (wbase + kq * (BN * 4)).bitcast[Int32]().load[
                    width=TN, alignment=16
                ]()

                comptime for m in range(TM):
                    comptime for r in range(TN):
                        isum[m * TN + r] = dot4_i8(
                            av[m], bv[r], isum[m * TN + r]
                        )

            comptime for m in range(TM):
                dx = xds[kb * BM + ty * TM + m]
                comptime for r in range(TN):
                    acc[m * TN + r] += (
                        dx * wds[kb * BN + tx * TN + r] * Float32(
                            isum[m * TN + r]
                        )
                    )
        barrier()
        base_blk += KB

    comptime for m in range(TM):
        t = t0 + ty * TM + m
        if t < n_tokens:
            comptime for r in range(TN):
                row = row0 + tx * TN + r
                if row < n_rows:
                    y[t * n_rows + row] = acc[m * TN + r].cast[OUT]()


comptime gemm_q4_0_dot4_64x64 = gemm_q4_0_dot4_impl[
    64, 64, 4, 4, 4, DType.float16
]
comptime gemm_q4_0_dot4_128x64 = gemm_q4_0_dot4_impl[
    128, 64, 8, 4, 2, DType.float16
]
comptime gemm_q4_0_dot4_128x128 = gemm_q4_0_dot4_impl[
    128, 128, 8, 4, 2, DType.float16
]
comptime gemm_q4_0_dot4_out_f32_64x64 = gemm_q4_0_dot4_impl[
    64, 64, 4, 4, 4, DType.float32
]
