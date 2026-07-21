# ===== File: prefill.mojo — batched KV append + causal prefill attention (paged) =====

from std.gpu import block_dim, block_idx, thread_idx
from std.gpu.sync import barrier
from std.gpu.primitives import warp
from std.gpu.primitives.warp import shuffle_xor
from std.gpu.memory import AddressSpace
from std.memory import stack_allocation
from std.math import exp
from std.gpu.compute.mma import mma, ld_matrix
from src.kv_fp8 import kv_row8_f16

comptime WARP_SIZE = 32
comptime MAX_WARPS = 8
comptime NEG_INF: Float32 = -1e30


def kv_append_batch[kv_dtype: DType](
    k_cache: UnsafePointer[Scalar[kv_dtype], MutAnyOrigin],
    v_cache: UnsafePointer[Scalar[kv_dtype], MutAnyOrigin],
    k_in: UnsafePointer[Float16, MutAnyOrigin],
    v_in: UnsafePointer[Float16, MutAnyOrigin],
    page_table: UnsafePointer[Int32, MutAnyOrigin],
    base_pos: Int,
    n_kv_heads: Int,
    page_size: Int,
    head_dim: Int,
):
    """Scatter T tokens' K/V ([T, n_kv_heads, head_dim]) at positions
    base_pos..base_pos+T. Grid: (T, n_kv_heads). kv_dtype = float16 stores
    the f16 rows verbatim; float8_e4m3fn casts per value (RN, satfinite) —
    the scale-free e4m3 range (±448, min denormal 2^-9) covers post-norm
    K/V magnitudes."""
    tok = Int(block_idx.x)
    kvh = Int(block_idx.y)
    pos = base_pos + tok
    page = Int(page_table[pos // page_size])
    slot = pos % page_size

    dst = ((page * n_kv_heads + kvh) * page_size + slot) * head_dim
    src = (tok * n_kv_heads + kvh) * head_dim

    var i = Int(thread_idx.x)
    while i < head_dim:
        k_cache[dst + i] = Scalar[kv_dtype](Float32(k_in[src + i]))
        v_cache[dst + i] = Scalar[kv_dtype](Float32(v_in[src + i]))
        i += Int(block_dim.x)


comptime kv_append_batch_f16 = kv_append_batch[DType.float16]
comptime kv_append_batch_fp8 = kv_append_batch[DType.float8_e4m3fn]


def kv_append_batch_device_pos_f16(
    k_cache: UnsafePointer[Float16, MutAnyOrigin],
    v_cache: UnsafePointer[Float16, MutAnyOrigin],
    k_in: UnsafePointer[Float16, MutAnyOrigin],
    v_in: UnsafePointer[Float16, MutAnyOrigin],
    page_table: UnsafePointer[Int32, MutAnyOrigin],
    base_pos: UnsafePointer[Int32, MutAnyOrigin],
    n_kv_heads: Int,
    page_size: Int,
    head_dim: Int,
):
    """Wariant F16 odczytujący bazową pozycję z bufora urządzenia."""
    kv_append_batch[DType.float16](
        k_cache, v_cache, k_in, v_in, page_table, Int(base_pos[0]),
        n_kv_heads, page_size, head_dim,
    )


comptime QT = 16  # query tokens per block
comptime PT = WARP_SIZE  # cached positions per smem tile (one lane per position)
comptime QPW = QT // MAX_WARPS  # queries owned by one warp


def attn_prefill[head_dim: Int, kv_dtype: DType](
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    q: UnsafePointer[Float16, MutAnyOrigin],
    k_cache: UnsafePointer[Scalar[kv_dtype], MutAnyOrigin],
    v_cache: UnsafePointer[Scalar[kv_dtype], MutAnyOrigin],
    page_table: UnsafePointer[Int32, MutAnyOrigin],
    base_pos: Int,
    n_q_heads: Int,
    n_kv_heads: Int,
    page_size: Int,
    scale: Float32,
    n_tokens: Int,
):
    """Causal attention for a prefill chunk over the paged cache.

    q/out: [T, n_q_heads, head_dim]; query token tok attends positions
    0..base_pos+tok (its K/V must already be in the cache).
    Grid: (ceil(T/QT), heads), block 256. K/V stream through shared-memory
    tiles of PT positions, so each cache byte is fetched once per QT queries
    instead of once per query. Each warp owns QPW queries; per tile, lane j
    scores position j (K rows XOR-swizzled per 16-byte chunk so lane-strided
    row reads are bank-conflict-free), the softmax exp runs 32-wide, the
    online-softmax rescale happens once per TILE (not per position), and the
    P·V accumulation broadcasts p_j via warp shuffle with lanes splitting the
    head dimension.

    kv_dtype = float8_e4m3fn reads an FP8 cache: rows are widened to f16 in
    the shared-memory tiles (e4m3 values are exactly representable in f16),
    so the attention arithmetic is bit-identical to the f16 kernel run on a
    dequantized cache."""
    comptime epl = head_dim // WARP_SIZE
    comptime row_chunks = head_dim // 8
    comptime tile_chunks = PT * row_chunks
    comptime block_threads = MAX_WARPS * WARP_SIZE
    comptime chunks_per_thread = tile_chunks // block_threads
    comptime q_chunks = QT * row_chunks

    tok0 = Int(block_idx.x) * QT
    qh = Int(block_idx.y)
    kvh = qh // (n_q_heads // n_kv_heads)
    tid = Int(thread_idx.x)
    lane = tid % WARP_SIZE
    wid = tid // WARP_SIZE

    qs = stack_allocation[
        QT * head_dim, Float16, address_space = AddressSpace.SHARED
    ]()
    ks = stack_allocation[
        PT * head_dim, Float16, address_space = AddressSpace.SHARED
    ]()
    vs = stack_allocation[
        PT * head_dim, Float16, address_space = AddressSpace.SHARED
    ]()

    # Stage the block's Q tile once; rows are read chunk-broadcast (all lanes
    # of a warp hit the same address), so no swizzle is needed. Out-of-range
    # token rows duplicate the last row — their queries are masked off below.
    comptime for it in range((q_chunks + block_threads - 1) // block_threads):
        c = tid + it * block_threads
        if c < q_chunks:
            row = c // row_chunks
            off = (c % row_chunks) * 8
            var tq = tok0 + row
            if tq > n_tokens - 1:
                tq = n_tokens - 1
            (qs + row * head_dim + off).store[width=8, alignment=16](
                (q + (tq * n_q_heads + qh) * head_dim + off).load[
                    width=8, alignment=16
                ]()
            )

    var m = InlineArray[Float32, QPW](fill=NEG_INF)
    var l = InlineArray[Float32, QPW](fill=0.0)
    var acc = InlineArray[SIMD[DType.float32, epl], QPW](
        fill=SIMD[DType.float32, epl](0.0)
    )

    # Highest position any query in this block attends.
    var tok_hi = tok0 + QT
    if tok_hi > n_tokens:
        tok_hi = n_tokens
    max_abs = base_pos + tok_hi - 1

    var pos0 = 0
    while pos0 <= max_abs:
        var n_valid = max_abs + 1 - pos0
        if n_valid > PT:
            n_valid = PT
        barrier()

        comptime for it in range(chunks_per_thread):
            c = tid + it * block_threads
            row = c // row_chunks
            off = c % row_chunks
            if row < n_valid:
                pos = pos0 + row
                page = Int(page_table[pos // page_size])
                kv_base = (
                    (page * n_kv_heads + kvh) * page_size + pos % page_size
                ) * head_dim + off * 8
                (ks + row * head_dim + ((off ^ (row % row_chunks)) * 8)).store[
                    width=8, alignment=16
                ](kv_row8_f16[kv_dtype](k_cache + kv_base))
                (vs + row * head_dim + off * 8).store[width=8, alignment=16](
                    kv_row8_f16[kv_dtype](v_cache + kv_base)
                )
        barrier()

        comptime for i in range(QPW):
            tq = tok0 + wid * QPW + i
            var h = base_pos + tq - pos0 + 1
            if tq >= n_tokens:
                h = 0
            if h > n_valid:
                h = n_valid
            if h > 0:
                # Lane `lane` scores position pos0+lane against query i.
                var dotv = SIMD[DType.float32, 8](0.0)

                comptime for c in range(row_chunks):
                    kv8 = (
                        ks
                        + lane * head_dim
                        + ((c ^ (lane % row_chunks)) * 8)
                    ).load[width=8, alignment=16]().cast[DType.float32]()
                    qv8 = (
                        qs + (wid * QPW + i) * head_dim + c * 8
                    ).load[width=8, alignment=16]().cast[DType.float32]()
                    dotv += qv8 * kv8
                var score = dotv.reduce_add() * scale
                if lane >= h:
                    score = NEG_INF
                mtile = warp.max(score)
                if mtile > m[i]:
                    rescale = exp(m[i] - mtile)
                    l[i] *= rescale
                    acc[i] = acc[i] * rescale
                    m[i] = mtile
                p = exp(score - m[i])
                l[i] += warp.sum(p)

                var jj = 0
                while jj < h:
                    pj = warp.shuffle_idx(p, UInt32(jj))
                    vfj = (vs + jj * head_dim + lane * epl).load[
                        width=epl, alignment = epl * 2
                    ]().cast[DType.float32]()
                    acc[i] += vfj * pj
                    jj += 1

        pos0 += PT

    comptime for i in range(QPW):
        tq = tok0 + wid * QPW + i
        if tq < n_tokens:
            q_base = (tq * n_q_heads + qh) * head_dim
            inv_l = 1.0 / l[i]
            (out_ptr + q_base + lane * epl).store[width=epl](
                (acc[i] * inv_l).cast[DType.float16]()
            )


comptime attn_prefill_f16_hd64 = attn_prefill[64, DType.float16]
comptime attn_prefill_f16_hd128 = attn_prefill[128, DType.float16]
comptime attn_prefill_f16_hd256 = attn_prefill[256, DType.float16]
comptime attn_prefill_fp8_hd64 = attn_prefill[64, DType.float8_e4m3fn]
comptime attn_prefill_fp8_hd128 = attn_prefill[128, DType.float8_e4m3fn]


def attn_prefill_device_pos_f16_hd256(
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
    """Wariant HD256 odczytujący bazową pozycję z bufora urządzenia."""
    attn_prefill[256, DType.float16](
        out_ptr, q, k_cache, v_cache, page_table, Int(base_pos[0]),
        n_q_heads, n_kv_heads, page_size, scale, n_tokens,
    )


comptime BQ = 64  # query rows per block (4 warps x 16)
comptime BK = 32  # cached positions per KV tile


def attn_prefill_fa_mma[head_dim: Int](
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
    """Tensor-core causal flash-attention prefill over the paged KV cache.

    Drop-in for the scalar `attn_prefill` (same f16 I/O contract) but QK^T and
    P·V run as f16 `m16n8k16` mma with an online softmax kept in registers —
    the Mojo mirror of kernels/cuda/fattn_prefill.cu. Grid: (ceil(T/64), heads),
    block 128 (4 warps). Each warp owns a 16-query m-tile; K/V stream through
    smem tiles of BK positions (V stored transposed [head_dim][key] so the P·V
    mma reads it non-transposed). Online-softmax state: lane owns query rows
    ra=lane/4 and rb=lane/4+8 (mma D layout pairs along n)."""
    comptime HD = head_dim
    comptime KC = HD // 16  # QK^T k-chunks (head_dim / 16)
    comptime NSUB = BK // 8  # S n-subtiles (8 keys each)
    comptime KCK = BK // 16  # P·V k-chunks (16 keys each)
    comptime HN = HD // 8  # O n-subtiles (8 head-dims each)
    comptime BLOCK = 128

    tid = Int(thread_idx.x)
    warp_id = tid // WARP_SIZE
    lane = tid % WARP_SIZE
    lr = lane & 7
    sub = lane >> 3
    q0 = Int(block_idx.x) * BQ
    qh = Int(block_idx.y)
    kvh = qh // (n_q_heads // n_kv_heads)
    mrow = q0 + warp_id * 16

    qs = stack_allocation[BQ * HD, Float16, address_space = AddressSpace.SHARED]()
    ks = stack_allocation[BK * HD, Float16, address_space = AddressSpace.SHARED]()
    vs = stack_allocation[HD * BK, Float16, address_space = AddressSpace.SHARED]()

    # Stage the block's Q tile once (rows past n_tokens duplicate the last, then
    # get masked at write-out).
    var e = tid * 8
    while e < BQ * HD:
        row = e // HD
        col = e % HD
        var tq = q0 + row
        if tq > n_tokens - 1:
            tq = n_tokens - 1
        (qs + e).store[width=8, alignment=16](
            (q + (tq * n_q_heads + qh) * HD + col).load[width=8, alignment=16]()
        )
        e += BLOCK * 8
    barrier()

    # Preload this warp's Q fragments (reused across every KV tile). A operand
    # 16x16 per k-chunk (row-major [m][k]): identical addressing to the GEMM's
    # non-transposed ld_matrix.x4.
    grp = lane >> 3
    qrow = warp_id * 16 + (lane & 7) + ((grp & 1) << 3)
    qcol = (grp >> 1) << 3
    var qf = InlineArray[SIMD[DType.float16, 8], KC](
        fill=SIMD[DType.float16, 8](0.0)
    )

    comptime for kc in range(KC):
        qf[kc] = ld_matrix[8](qs + qrow * HD + kc * 16 + qcol)

    # Online-softmax state: each lane owns rows ra=lane/4 and rb=lane/4+8.
    var m_a = NEG_INF
    var m_b = NEG_INF
    var l_a: Float32 = 0.0
    var l_b: Float32 = 0.0
    var acc = InlineArray[SIMD[DType.float32, 4], HN](
        fill=SIMD[DType.float32, 4](0.0)
    )

    var tok_hi = q0 + BQ
    if tok_hi > n_tokens:
        tok_hi = n_tokens
    max_abs = base_pos + tok_hi - 1

    var pos0 = 0
    while pos0 <= max_abs:
        var n_valid = max_abs + 1 - pos0
        if n_valid > BK:
            n_valid = BK
        barrier()

        # Stage K/V tile [BK][HD] from the paged cache. K stays [key][head_dim];
        # V is written transposed to [head_dim][key] so the P·V mma reads it
        # non-transposed.
        var e2 = tid * 8
        while e2 < BK * HD:
            row = e2 // HD
            col = e2 % HD
            pos = pos0 + row
            if row < n_valid:
                page = Int(page_table[pos // page_size])
                kv_base = (
                    (page * n_kv_heads + kvh) * page_size + pos % page_size
                ) * HD + col
                (ks + e2).store[width=8, alignment=16](
                    (k_cache + kv_base).load[width=8, alignment=16]()
                )
                vv = (v_cache + kv_base).load[width=8, alignment=16]()

                comptime for i in range(8):
                    vs[(col + i) * BK + row] = vv[i]
            else:
                (ks + e2).store[width=8, alignment=16](
                    SIMD[DType.float16, 8](0.0)
                )

                comptime for i in range(8):
                    vs[(col + i) * BK + row] = Float16(0.0)
            e2 += BLOCK * 8
        barrier()

        # S = Q * K^T for this tile: NSUB fragments of 8 keys each.
        var s = InlineArray[SIMD[DType.float32, 4], NSUB](
            fill=SIMD[DType.float32, 4](0.0)
        )

        comptime for nt in range(NSUB):

            comptime for kc in range(KC):
                bf = ld_matrix[4](
                    ks + (nt * 8 + lr) * HD + kc * 16 + (sub & 1) * 8
                )
                mma(s[nt], qf[kc], bf, s[nt])

        # Online softmax. Lane owns row ra=lane/4 (s cols d0,d1) and rb=lane/4+8
        # (d2,d3). Mask tile tail + causal horizon, reduce max over the tile.
        var local_a = NEG_INF
        var local_b = NEG_INF
        ga = q0 + warp_id * 16 + (lane >> 2)
        gb = ga + 8
        ha = base_pos + ga - pos0
        hb = base_pos + gb - pos0

        comptime for nt in range(NSUB):
            key_a = nt * 8 + (lane & 3) * 2
            var sv = s[nt]
            if key_a >= n_valid or key_a > ha:
                sv[0] = NEG_INF
            if key_a + 1 >= n_valid or key_a + 1 > ha:
                sv[1] = NEG_INF
            if key_a >= n_valid or key_a > hb:
                sv[2] = NEG_INF
            if key_a + 1 >= n_valid or key_a + 1 > hb:
                sv[3] = NEG_INF
            sv[0] *= scale
            sv[1] *= scale
            sv[2] *= scale
            sv[3] *= scale
            s[nt] = sv
            local_a = max(local_a, max(sv[0], sv[1]))
            local_b = max(local_b, max(sv[2], sv[3]))

        # Reduce across the 4 lanes sharing a row (lane&3 = 0..3).
        local_a = max(local_a, shuffle_xor(local_a, 1))
        local_a = max(local_a, shuffle_xor(local_a, 2))
        local_b = max(local_b, shuffle_xor(local_b, 1))
        local_b = max(local_b, shuffle_xor(local_b, 2))

        new_m_a = max(m_a, local_a)
        new_m_b = max(m_b, local_b)
        corr_a = Float32(1.0) if m_a <= NEG_INF else exp(m_a - new_m_a)
        corr_b = Float32(1.0) if m_b <= NEG_INF else exp(m_b - new_m_b)
        m_a = new_m_a
        m_b = new_m_b
        l_a *= corr_a
        l_b *= corr_b

        comptime for i in range(HN):
            var av = acc[i]
            av[0] *= corr_a
            av[1] *= corr_a
            av[2] *= corr_b
            av[3] *= corr_b
            acc[i] = av

        # exp(S - m) in place -> P; accumulate row sums.
        var sum_a: Float32 = 0.0
        var sum_b: Float32 = 0.0

        comptime for nt in range(NSUB):
            var sv = s[nt]
            var p0 = exp(sv[0] - m_a)
            var p1 = exp(sv[1] - m_a)
            var p2 = exp(sv[2] - m_b)
            var p3 = exp(sv[3] - m_b)
            sv[0] = p0
            sv[1] = p1
            sv[2] = p2
            sv[3] = p3
            s[nt] = sv
            sum_a += p0 + p1
            sum_b += p2 + p3
        sum_a += shuffle_xor(sum_a, 1)
        sum_a += shuffle_xor(sum_a, 2)
        sum_b += shuffle_xor(sum_b, 1)
        sum_b += shuffle_xor(sum_b, 2)
        l_a += sum_a
        l_b += sum_b

        # O += P * V. P (16 query x BK key) reinterpreted as mma A operand: the
        # S accumulator layout equals the A-operand layout, so pack the f32 probs
        # to f16 (h2 pairs). B = V (transposed in smem, non-transposed ld_matrix).
        comptime for kck in range(KCK):
            s0 = s[2 * kck]
            s1 = s[2 * kck + 1]
            var pf = SIMD[DType.float16, 8](0.0)
            pf[0] = Float16(s0[0])
            pf[1] = Float16(s0[1])
            pf[2] = Float16(s0[2])
            pf[3] = Float16(s0[3])
            pf[4] = Float16(s1[0])
            pf[5] = Float16(s1[1])
            pf[6] = Float16(s1[2])
            pf[7] = Float16(s1[3])

            comptime for hn in range(HN):
                bv = ld_matrix[4](
                    vs + (hn * 8 + lr) * BK + kck * 16 + (sub & 1) * 8
                )
                mma(acc[hn], pf, bv, acc[hn])

        pos0 += BK

    # Write O = acc / l. Lane owns rows ra=mrow+lane/4, rb=ra+8; each O n-subtile
    # holds cols (lane%4)*2, +1 in d0,d1 (ra) and d2,d3 (rb).
    ra = mrow + (lane >> 2)
    rb = ra + 8
    inv_a = (1.0 / l_a) if l_a > 0.0 else Float32(0.0)
    inv_b = (1.0 / l_b) if l_b > 0.0 else Float32(0.0)

    comptime for hn in range(HN):
        col = hn * 8 + (lane & 3) * 2
        av = acc[hn]
        if ra < n_tokens:
            o = (ra * n_q_heads + qh) * HD + col
            out_ptr[o] = Float16(av[0] * inv_a)
            out_ptr[o + 1] = Float16(av[1] * inv_a)
        if rb < n_tokens:
            o = (rb * n_q_heads + qh) * HD + col
            out_ptr[o] = Float16(av[2] * inv_b)
            out_ptr[o + 1] = Float16(av[3] * inv_b)


comptime attn_prefill_fa_f16_hd64 = attn_prefill_fa_mma[64]
comptime attn_prefill_fa_f16_hd128 = attn_prefill_fa_mma[128]
