# =============================================================================
# Plik: prefill_wmma.mojo
# Opis: Flash attention prefillu na jednostce macierzowej, KV f16. `head_dim`
#       jest PARAMETREM KOMPILACJI, nie stala modulu — kazdy model ma swoje.
#       Q·Kᵀ i P·V liczy WMMA 16x16x16 zamiast iloczynów skalarnych na linię.
# Przykład: attn_prefill_wmma_hd128(out, q, k, v, page_table, ...)
# =============================================================================
#
# Dlaczego osobny kernel, a nie `comptime if` w `attn_prefill`: tamten trzyma
# JEDEN klucz na linię i liczy pełny iloczyn 128-elementowy, a WMMA liczy kafel
# 16x16 naraz. Różni się cały rozkład pracy między liniami, a nie instrukcja —
# tego nie da się schować za rozgałęzieniem.
#
# UKŁADY, na których to stoi (zmierzone na karcie, patrz `arch_wmma.mojo`):
#   A: linia trzyma A[wiersz = lane%16][k = 8*(lane/16) + e]
#   B: linia trzyma B[k = 8*(lane/16) + e][kolumna = lane%16]
#   D: linia trzyma D[wiersz = 8*(lane/16) + e][kolumna = lane%16]
#
# Stąd jedyna nieoczywistość: wynik Q·Kᵀ wychodzi w układzie D (zapytanie
# rozrzucone po `e`), a mnożenie P·V potrzebuje P w układzie A (zapytanie w
# `lane%16`). Przejście między nimi idzie przez pamięć współdzieloną — 512 B na
# falę, czyli koszt pomijalny wobec kafla.

from std.gpu import block_dim, block_idx, thread_idx
from std.gpu.sync import barrier
from std.gpu.primitives import warp
from std.gpu.primitives.warp import shuffle_xor
from std.gpu.memory import AddressSpace
from std.memory import stack_allocation
from std.math import exp
from src.arch_wmma import wmma_f16_16x16x16_native

comptime WARP: Int = 32
comptime TILE: Int = 16
comptime WAVES: Int = 4
comptime QT: Int = WAVES * TILE
comptime NEG_INF: Float32 = -3.0e38


def attn_prefill_wmma_impl[HD: Int](
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    q: UnsafePointer[Float16, MutAnyOrigin],
    k_cache: UnsafePointer[Float16, MutAnyOrigin],
    v_cache: UnsafePointer[Float16, MutAnyOrigin],
    page_table: UnsafePointer[Int32, MutAnyOrigin],
    base_pos: Int,
    n_q_heads: Int,
    n_kv_heads: Int,
    page_size: Int,
    scale: Float32,
    n_tokens: Int,
):
    """Causal attention prefillu nad stronicowanym cache, kafel 16x16 na WMMA.

    Siatka: (ceil(T/64), n_q_heads), blok 128 wątków (cztery fale). Każda fala
    liczy 16 zapytań; kafel K/V 16 pozycji leży w pamięci współdzielonej i jest
    czytany przez wszystkie cztery fale, więc bajt cache'u pobiera się raz na 64
    zapytania.
    """
    comptime CHUNKS: Int = HD // TILE
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    tid = Int(thread_idx.x)
    col = lane % TILE
    half = lane // TILE

    qh = Int(block_idx.y)
    kvh = qh * n_kv_heads // n_q_heads
    tok0 = Int(block_idx.x) * QT + wid * TILE

    ks = stack_allocation[
        TILE * HD, Float16, address_space = AddressSpace.SHARED
    ]()
    vs = stack_allocation[
        TILE * HD, Float16, address_space = AddressSpace.SHARED
    ]()
    ps = stack_allocation[
        WAVES * TILE * TILE, Float16, address_space = AddressSpace.SHARED
    ]()

    # Fragment Q zostaje w rejestrach na cały przebieg: linia niesie wiersz
    # `col` kafla zapytań, po osiem wartości z każdego z ośmiu kawałków wymiaru
    # głowicy.
    var qf = InlineArray[SIMD[DType.float16, 8], CHUNKS](
        fill=SIMD[DType.float16, 8](0)
    )
    tq = tok0 + col
    if tq < n_tokens:
        qbase = (tq * n_q_heads + qh) * HD + half * 8
        comptime for c in range(CHUNKS):
            qf[c] = (q + qbase + c * TILE).load[width=8, alignment=16]()

    var acc = InlineArray[SIMD[DType.float32, 8], CHUNKS](
        fill=SIMD[DType.float32, 8](0.0)
    )
    var m = SIMD[DType.float32, 8](NEG_INF)
    var lsum = SIMD[DType.float32, 8](0.0)

    # Najdalsza pozycja, jaką widzi którekolwiek zapytanie tego bloku.
    tok_hi = Int(block_idx.x) * QT + QT
    if tok_hi > n_tokens:
        tok_hi = n_tokens
    max_abs = base_pos + tok_hi - 1

    var key0 = 0
    while key0 <= max_abs:
        barrier()
        # Kafel K/V: 16 pozycji x 128 wartości, 128 wątków po dwie ósemki.
        comptime for it in range(TILE * HD // (WAVES * WARP * 8)):
            c = tid + it * (WAVES * WARP)
            row = c // (HD // 8)
            off = (c % (HD // 8)) * 8
            pos = key0 + row
            if pos <= max_abs:
                page = Int(page_table[pos // page_size])
                kv = (
                    (page * n_kv_heads + kvh) * page_size + pos % page_size
                ) * HD + off
                (ks + row * HD + off).store[width=8, alignment=16](
                    (k_cache + kv).load[width=8, alignment=16]()
                )
                (vs + row * HD + off).store[width=8, alignment=16](
                    (v_cache + kv).load[width=8, alignment=16]()
                )
            else:
                (ks + row * HD + off).store[width=8, alignment=16](
                    SIMD[DType.float16, 8](0)
                )
                (vs + row * HD + off).store[width=8, alignment=16](
                    SIMD[DType.float16, 8](0)
                )
        barrier()

        # S = Q·Kᵀ. Operand B to Kᵀ: linia bierze wiersz `col` kafla kluczy,
        # czyli B[k][kolumna=col] = K[klucz=col][k].
        var s = SIMD[DType.float32, 8](0.0)
        comptime for c in range(CHUNKS):
            kf = (ks + col * HD + c * TILE + half * 8).load[
                width=8, alignment=16
            ]()
            s = wmma_f16_16x16x16_native(qf[c], kf, s)

        # Linia trzyma teraz S[zapytanie = 8*half + e][klucz = col].
        var sc = s * scale
        comptime for e in range(8):
            tqa = base_pos + tok0 + 8 * half + e
            if tok0 + 8 * half + e >= n_tokens or key0 + col > tqa:
                sc[e] = NEG_INF

        # Maksimum i suma PO WIERSZU, czyli po 16 liniach tej samej połowy fali.
        # Wiersz kafla lezy w 16 liniach tej samej polowy fali, wiec redukcja
        # idzie motylkiem po maskach 1,2,4,8 — nigdy nie przekracza polowy.
        var rmax = sc
        comptime for e in range(8):
            var v = sc[e]
            comptime for stride in range(4):
                v = max(v, shuffle_xor(v, UInt32(1 << stride)))
            rmax[e] = v

        var mnew = max(m, rmax)
        var rescale = exp(m - mnew)
        comptime for e in range(8):
            if m[e] == NEG_INF:
                rescale[e] = 0.0
        var p = exp(sc - mnew)
        comptime for e in range(8):
            if mnew[e] == NEG_INF:
                p[e] = 0.0
        var rsum = p
        comptime for e in range(8):
            var v = p[e]
            comptime for stride in range(4):
                v += shuffle_xor(v, UInt32(1 << stride))
            rsum[e] = v
        lsum = lsum * rescale + rsum
        m = mnew
        comptime for c in range(CHUNKS):
            acc[c] = acc[c] * rescale

        # P wychodzi w układzie D, a mnożenie P·V potrzebuje go w układzie A —
        # przejście idzie przez pamięć współdzieloną tej fali.
        barrier()
        pw = ps + wid * TILE * TILE
        comptime for e in range(8):
            pw[(8 * half + e) * TILE + col] = p[e].cast[DType.float16]()
        barrier()
        pf = (pw + col * TILE + half * 8).load[width=8, alignment=16]()

        # O += P·V. Operand B to V: linia bierze kolumnę `col` kawałka głowicy.
        comptime for c in range(CHUNKS):
            var vf = SIMD[DType.float16, 8](0)
            comptime for e in range(8):
                vf[e] = vs[(8 * half + e) * HD + c * TILE + col]
            acc[c] = wmma_f16_16x16x16_native(pf, vf, acc[c])

        key0 += TILE

    # Wynik: linia trzyma O[zapytanie = 8*half + e][kolumna = col].
    comptime for e in range(8):
        t = tok0 + 8 * half + e
        if t < n_tokens and lsum[e] > 0.0:
            inv = 1.0 / lsum[e]
            base = (t * n_q_heads + qh) * HD
            comptime for c in range(CHUNKS):
                out_ptr[base + c * TILE + col] = (acc[c][e] * inv).cast[
                    DType.float16
                ]()


# `head_dim` NALEZY DO MODELU, nie do kernela. Instancje sa tu jawne, a launcher
# wybiera je po `params.head_dim`, wiec dolozenie kolejnego ksztaltu to dopisanie
# aliasu i wpisu w katalogu — nie edycja stalej, ktora zepsulaby pozostale modele.
# Bielik i Mistral maja 128, ThinkingCap-Qwen3.6-27B ma 256.
comptime attn_prefill_wmma_hd128 = attn_prefill_wmma_impl[128]
comptime attn_prefill_wmma_hd256 = attn_prefill_wmma_impl[256]


def attn_prefill_wmma_pos_impl[HD: Int](
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    q: UnsafePointer[Float16, MutAnyOrigin],
    k_cache: UnsafePointer[Float16, MutAnyOrigin],
    v_cache: UnsafePointer[Float16, MutAnyOrigin],
    page_table: UnsafePointer[Int32, MutAnyOrigin],
    base_pos: UnsafePointer[Int32, MutAnyOrigin],
    n_q_heads: Int,
    n_kv_heads: Int,
    page_size: Int,
    scale: Float32,
    n_tokens: Int,
):
    """Ten sam kafel, ale pozycja bazowa leży na urządzeniu.

    Prefill layer-major trzyma długość sekwencji w buforze GPU, bo chunk rusza
    zanim host pozna jej wartość. Odczyt jest skalarny i jednakowy dla całego
    bloku, więc kompilator trzyma go w rejestrze skalarnym — kafel pracuje bez
    zmian.
    """
    attn_prefill_wmma_impl[HD](
        out_ptr, q, k_cache, v_cache, page_table, Int(base_pos[0]),
        n_q_heads, n_kv_heads, page_size, scale, n_tokens,
    )


comptime attn_prefill_wmma_pos_hd128 = attn_prefill_wmma_pos_impl[128]
comptime attn_prefill_wmma_pos_hd256 = attn_prefill_wmma_pos_impl[256]
