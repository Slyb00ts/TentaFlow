# ===----------------------------------------------------------------------=== #
# Copyright (c) 2026, Modular Inc. All rights reserved.
#
# Licensed under the Apache License v2.0 with LLVM Exceptions:
# https://llvm.org/LICENSE.txt
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.
# ===----------------------------------------------------------------------=== #
#
# NATIVE-LAYOUT Q6_K int8 multistage GEMM — reads the raw GGUF `block_q6_K`
# superblock bytes IN-KERNEL (210 bytes / 256 weights: ql[128] low nibbles,
# qh[64] 2-bit highs, int8 scales[16] per-16-block, half d). It consumes the
# EXACT same `DevWeight::Q6K.buf` bytes the decode dp4a GEMV reads (true 1× VRAM),
# so there is NO separate weight copy and NO pre-repacked scale/weight tensor.
#
# Forked from `multistage_i8_q4k_native.mojo`. The int8 mma stays m16n8k32, but
# Q6_K's scale granularity is 16 (not 32), so a single k=32 mma spans TWO scale
# sub-blocks. To keep the flush bit-exact we run the mma TWICE per 32-region: once
# on the full B fragment (S_full = S_lo + S_hi) and once on the fragment with its
# upper-16-k half zeroed (S_lo). S_hi = S_full − S_lo. The per-32-region flush is
#   acc += da · (dsc_lo · S_lo + dsc_hi · S_hi)
# with dsc = d·scale[16-block] and da the q8_1 per-32-block activation scale. The
# Q6_K value d·sc·(q−32) folds the −32 offset into the int8 weight code
# (code = q6 − 32 ∈ [−32, 31]), so there is NO min term (sa unused). Result is
# bit-identical to a CPU Q6_K × q8_1 golden by construction.

from std.math import ceildiv
from std.math.uutils import umod, ufloordiv, udivmod, uceildiv
from std.sys import align_of, is_nvidia_gpu, simd_width_of, size_of

from std.gpu import (
    MAX_THREADS_PER_BLOCK_METADATA,
    WARP_SIZE,
    barrier,
    block_idx,
    grid_dim,
    lane_id,
    thread_idx,
)
from std.gpu.memory import (
    async_copy_commit_group,
    async_copy_wait_group,
    external_memory,
)
from std.memory.unsafe import bitcast
from layout.layout import *
from layout import (
    Coord,
    Idx,
    LayoutTensor,
    RuntimeLayout,
    RuntimeTuple,
    TensorLayout,
    TileTensor,
)
from layout.layout_tensor import (
    LayoutTensorIter,
    copy_dram_to_sram_async,
    copy_local_to_dram,
)
from layout.swizzle import Swizzle, make_ldmatrix_swizzle
from .tensor_core_i8 import TensorCore, get_fragment_size, get_mma_shape
from .tensor_core_i8 import _mma_s8_frag

from std.utils.index import Index, IndexList

from linalg.utils_gpu import MatmulConfig


@always_inline
def multistage_mma_q6k_native[
    a_type: DType,
    a_layout: Layout,
    a_smem_layout: Layout,
    b_smem_layout: Layout,
    //,
    BM: Int,
    BN: Int,
    BK: Int,
    WM: Int,
    WN: Int,
    num_threads: Int,
    num_pipeline_stages: Int,
](
    acc: LayoutTensor[
        mut=True, DType.float32, _, address_space=AddressSpace.LOCAL, ...
    ],
    a_iter_arg: LayoutTensorIter[_, a_layout, ...],
    a_smem_iter_arg: LayoutTensorIter[
        mut=True, a_type, a_smem_layout, address_space=AddressSpace.SHARED, ...
    ],
    b_smem_full: LayoutTensor[
        mut=True, a_type, b_smem_layout, address_space=AddressSpace.SHARED, ...
    ],
    num_iters: Int,
    # native GGUF Q6_K weight bytes ([N, K/256 * 210]), read in-kernel.
    w: UnsafePointer[UInt8, ImmutAnyOrigin],
    rowbytes: Int,
    # per-32-block q8_1 activation scale (f32); staged into smem as f16.
    da: UnsafePointer[Float32, ImmutAnyOrigin],
    da_sm: UnsafePointer[
        Float16, MutAnyOrigin, address_space = AddressSpace.SHARED
    ],
    dsc_lo_sm: UnsafePointer[
        Float16, MutAnyOrigin, address_space = AddressSpace.SHARED
    ],
    dsc_hi_sm: UnsafePointer[
        Float16, MutAnyOrigin, address_space = AddressSpace.SHARED
    ],
    M: Int,
    N: Int,
    K: Int,
    block_m_base: Int,
    block_n_base: Int,
):
    comptime simd_size = simd_width_of[a_type]()
    comptime transpose_b = True

    var full_tid = thread_idx.x
    var tid = UInt32(umod(thread_idx.x, num_threads))
    var warp_id = tid // UInt32(WARP_SIZE)
    var lane = lane_id()

    comptime num_warps_n = BN // WN
    var warp_y, warp_x = divmod(warp_id, UInt32(num_warps_n))

    var a_iter = a_iter_arg
    var a_smem_iter = a_smem_iter_arg

    comptime a_num_vecs = BM * BK // simd_size
    comptime async_copy_a_layout = Layout.row_major(
        min(num_threads, a_num_vecs) * simd_size // BK, BK // simd_size
    )

    @always_inline
    @parameter
    def _copy_a_to_sram(dst: LayoutTensor[mut=True, ...], src: LayoutTensor):
        copy_dram_to_sram_async[
            thread_layout=async_copy_a_layout,
            swizzle=True,
            num_threads=num_threads,
        ](dst.vectorize[1, simd_size](), src.vectorize[1, simd_size]())

    var b_base = b_smem_full.ptr

    @always_inline
    @parameter
    def _unpack_b(kt: Int, buf: Int):
        # Unpack this k-tile's native Q6_K 6-bit weights into the plain int8
        # buffer as (q6 − 32). BK=64 covers exactly one 32-col group-pair inside a
        # 128-half of the 256-wide superblock. Per (row n, k_local) we address the
        # ql/qh bytes directly (2-byte-aligned block → single-byte loads; the mma
        # bandwidth win dwarfs the uncoalesced unpack).
        var k0 = kt * BK
        var buf_off = buf * BN * BK
        var iv = Int(full_tid)
        comptime total = BN * BK
        while iv < total:
            var n = iv // BK
            var kl = iv % BK
            var gk = k0 + kl
            var superblock = gk // 256
            var p = gk % 256
            var hn = p // 128
            var wpos = p % 128
            var g = wpos // 32
            var jj = wpos % 32
            var grow = block_n_base + n
            var blkoff = grow * rowbytes + superblock * 210
            var qloff = blkoff + hn * 64 + (g & 1) * 32 + jj
            var qlshift = 4 if g >= 2 else 0
            var qhoff = blkoff + 128 + hn * 32 + jj
            var qhshift = 2 * g
            var ql = Int((w + qloff)[])
            var qh = Int((w + qhoff)[])
            var v6 = ((ql >> qlshift) & 0xF) | (((qh >> qhshift) & 3) << 4)
            b_base[buf_off + n * BK + kl] = Scalar[a_type](v6 - 32)
            iv += num_threads

    # Prefetch (num_pipeline_stages - 1) activation stages.
    comptime for stage in range(num_pipeline_stages - 1):
        var a_smem_tile = a_smem_iter.next_unsafe(
            a_smem_iter.linear_uint_type(stage)
        )[]
        _copy_a_to_sram(a_smem_tile, a_iter[])
        a_iter._incr()
        async_copy_commit_group()

    async_copy_wait_group(Int32(num_pipeline_stages - 2))
    barrier()

    _unpack_b(0, 0)
    barrier()

    comptime mma_shape = get_mma_shape[a_type, DType.int32]()
    comptime MMA_M = mma_shape[0]
    comptime MMA_N = mma_shape[1]
    comptime MMA_K = mma_shape[2]
    comptime num_k_mmas = BK // MMA_K
    comptime num_m_mmas = WM // MMA_M
    comptime num_n_mmas = WN // MMA_N

    comptime frag_size = get_fragment_size[mma_shape]()
    comptime a_frag_size = frag_size[0]
    comptime b_frag_size = frag_size[1]
    comptime c_frag_size = frag_size[2]

    comptime num_reg_tiles = 2
    comptime a_reg_layout = Layout.row_major(2 * num_m_mmas, a_frag_size)
    var a_reg_tiles = (
        LayoutTensor[
            a_type, a_reg_layout, MutAnyOrigin, address_space=AddressSpace.LOCAL
        ]
        .stack_allocation()
        .split[2]()
    )
    comptime b_reg_layout = Layout.row_major(2 * num_n_mmas, b_frag_size)
    var b_reg_tiles = (
        LayoutTensor[
            a_type, b_reg_layout, MutAnyOrigin, address_space=AddressSpace.LOCAL
        ]
        .stack_allocation()
        .vectorize[1, b_frag_size]()
        .split[2]()
    )

    var a_warp_tile = a_smem_iter[].tile[WM, BK](Int(warp_y), 0)
    var b_warp_tile = b_smem_full.tile[BN, BK](0, 0).tile[WN, BK](
        Int(warp_x), 0
    )

    var mma_op = TensorCore[
        DType.int32, a_type, mma_shape, transpose_b, b_swizzle=False
    ]()

    comptime swizzle_a_pattern = make_ldmatrix_swizzle[
        a_type, a_warp_tile.stride[0]()
    ]()

    var groupID = Int(lane) // 4
    var tgrp = Int(lane) % 4
    var warp_lrow = Int(warp_y) * WM
    var warp_lcol = Int(warp_x) * WN

    mma_op.load_a[swizzle_a_pattern](
        a_warp_tile, a_reg_tiles[0].vectorize[1, a_frag_size](), 0
    )
    mma_op.load_b(b_warp_tile, b_reg_tiles[0], 0, Int(warp_x))

    comptime row_stride = num_k_mmas * BM
    comptime col_stride = num_k_mmas * BN

    @always_inline
    @parameter
    def _stage_scales(k_tile_id: Int, buf: Int):
        var ro = buf * row_stride
        var co = buf * col_stride
        # activation scale da (per-32-block q8_1).
        var i = Int(full_tid)
        while i < row_stride:
            var sub = i // BM
            var lr = i % BM
            var kb = k_tile_id * num_k_mmas + sub
            var g = kb * M + block_m_base + lr
            da_sm[ro + i] = Float16(da[g])
            i += num_threads
        # weight scales dsc_lo/dsc_hi computed IN-KERNEL from the native header:
        # the 32-block kb holds two 16-scales at scales[2*jj] and scales[2*jj+1].
        var j = Int(full_tid)
        while j < col_stride:
            var sub = j // BN
            var lc = j % BN
            var kb = k_tile_id * num_k_mmas + sub
            var grow = block_n_base + lc
            var superblock = kb // 8
            var jj = kb % 8
            var blkoff = grow * rowbytes + superblock * 210
            var d = Float32((w + blkoff + 208).bitcast[Float16]()[0])
            var slo = Float32(Int((w + blkoff + 192 + 2 * jj).bitcast[Int8]()[0]))
            var shi = Float32(
                Int((w + blkoff + 192 + 2 * jj + 1).bitcast[Int8]()[0])
            )
            dsc_lo_sm[co + j] = Float16(d * slo)
            dsc_hi_sm[co + j] = Float16(d * shi)
            j += num_threads

    @always_inline
    @parameter
    def _flush(sub: Int, cur: Int, buf: Int):
        var ro = buf * row_stride + sub * BM
        var co = buf * col_stride + sub * BN
        var da_r = InlineArray[Float32, num_m_mmas * 2](fill=0)
        comptime for m_mma in range(num_m_mmas):
            comptime for h in range(2):
                var lr = warp_lrow + m_mma * MMA_M + groupID + 8 * h
                da_r[m_mma * 2 + h] = Float32(da_sm[ro + lr])
        var dsc_lo_r = InlineArray[Float32, num_n_mmas * 2](fill=0)
        var dsc_hi_r = InlineArray[Float32, num_n_mmas * 2](fill=0)
        comptime for n_mma in range(num_n_mmas):
            comptime for wv in range(2):
                var lc = warp_lcol + n_mma * MMA_N + tgrp * 2 + wv
                dsc_lo_r[n_mma * 2 + wv] = Float32(dsc_lo_sm[co + lc])
                dsc_hi_r[n_mma * 2 + wv] = Float32(dsc_hi_sm[co + lc])

        var zero4 = SIMD[DType.int32, 4](0)
        comptime for m_mma in range(num_m_mmas):
            var af = rebind[SIMD[DType.int8, 16]](
                a_reg_tiles[cur].vectorize[1, a_frag_size]()[m_mma, 0]
            )
            comptime for n_mma in range(num_n_mmas):
                var bf = rebind[SIMD[DType.int8, 8]](b_reg_tiles[cur][n_mma, 0])
                var bf_lo = bf
                bf_lo[4] = 0
                bf_lo[5] = 0
                bf_lo[6] = 0
                bf_lo[7] = 0
                var s_full = _mma_s8_frag(af, bf, zero4)
                var s_lo = _mma_s8_frag(af, bf_lo, zero4)
                comptime idx = n_mma * num_m_mmas + m_mma
                comptime for e in range(4):
                    comptime h = e // 2
                    comptime wv = e % 2
                    var v_lo = Float32(Int(s_lo[e]))
                    var v_hi = Float32(Int(s_full[e]) - Int(s_lo[e]))
                    acc[idx, e] += da_r[m_mma * 2 + h] * (
                        dsc_lo_r[n_mma * 2 + wv] * v_lo
                        + dsc_hi_r[n_mma * 2 + wv] * v_hi
                    )

    _stage_scales(0, 0)
    barrier()

    for k_tile_id in range(num_iters):
        var buf = k_tile_id % 2
        if k_tile_id + 1 < num_iters:
            _stage_scales(k_tile_id + 1, (k_tile_id + 1) % 2)

        var a_wt = a_smem_iter[].tile[WM, BK](Int(warp_y), 0)
        var b_wt = b_smem_full.tile[BN, BK](k_tile_id % 2, 0).tile[WN, BK](
            Int(warp_x), 0
        )

        comptime for k_mma in range(num_k_mmas):
            comptime k_mma_next = k_mma + 1
            comptime nxt = k_mma_next % num_reg_tiles

            comptime if k_mma_next == num_k_mmas:
                var prefetch_tile_id = k_tile_id + num_pipeline_stages - 1
                if prefetch_tile_id < num_iters:
                    var a_pf = a_smem_iter.next_unsafe(
                        a_smem_iter.linear_uint_type(num_pipeline_stages - 1)
                    )[]
                    _copy_a_to_sram(a_pf, a_iter[])
                    a_iter._incr()
                async_copy_commit_group()
                async_copy_wait_group(Int32(num_pipeline_stages - 2))

                a_smem_iter._incr()
                if k_tile_id + 1 < num_iters:
                    _unpack_b(k_tile_id + 1, (k_tile_id + 1) % 2)
                barrier()

                a_wt = a_smem_iter[].tile[WM, BK](Int(warp_y), 0)
                b_wt = b_smem_full.tile[BN, BK]((k_tile_id + 1) % 2, 0).tile[
                    WN, BK
                ](Int(warp_x), 0)

            comptime kidx = k_mma_next % num_k_mmas
            mma_op.load_a[swizzle_a_pattern](
                a_wt, a_reg_tiles[nxt].vectorize[1, a_frag_size](), kidx
            )
            mma_op.load_b(b_wt, b_reg_tiles[nxt], kidx, Int(warp_x))

            comptime cur = k_mma % num_reg_tiles
            _flush(k_mma, cur, buf)

        barrier()


@__name(t"multistage_gemm_q6k_native_kernel_{a_type}")
def multistage_gemm_q6k_native_kernel[
    CLT: TensorLayout,
    a_type: DType,
    ALT: TensorLayout,
    c_linear_idx_type: DType,
    a_linear_idx_type: DType,
    config: MatmulConfig[a_type, a_type, DType.float32, True, ...],
](
    c_tt: TileTensor[
        DType.float32, CLT, MutAnyOrigin, linear_idx_type=c_linear_idx_type
    ],
    a_tt: TileTensor[
        a_type, ALT, ImmutAnyOrigin, linear_idx_type=a_linear_idx_type
    ],
    w: UnsafePointer[UInt8, ImmutAnyOrigin],
    da: UnsafePointer[Float32, ImmutAnyOrigin],
    sa: UnsafePointer[Float32, ImmutAnyOrigin],
    y: UnsafePointer[Float16, MutAnyOrigin],
    m_real: Int,
):
    var c = c_tt.to_layout_tensor()
    var a = a_tt.to_layout_tensor()

    comptime assert a_type == DType.int8, "q6k pipeline only supports S8 mma"

    var M: Int = c.dim[0]()
    var N: Int = c.dim[1]()
    var K: Int = a.dim[1]()
    var rowbytes = (K // 256) * 210

    comptime BM = config.block_tile_shape[0]
    comptime BN = config.block_tile_shape[1]
    comptime BK = config.block_tile_shape[2]
    comptime WM = config.warp_tile_shape[0]
    comptime WN = config.warp_tile_shape[1]
    comptime num_pipeline_stages = config.num_pipeline_stages
    comptime num_warps_n = config.num_warps_n()
    comptime num_threads = config.num_threads()
    comptime num_k_mmas = BK // 32

    var tid = thread_idx.x
    var warp_id = ufloordiv(tid, WARP_SIZE)

    comptime alignment = align_of[SIMD[a_type, simd_width_of[a_type]()]]()
    var a_smem = external_memory[
        Scalar[a_type],
        address_space=AddressSpace.SHARED,
        alignment=alignment,
    ]()
    comptime a_smem_size: Int = num_pipeline_stages * BM * BK
    comptime IteratorTypeA = LayoutTensorIter[
        a_type,
        Layout.row_major(BM, BK),
        _,
        address_space = a_smem.address_space,
        alignment=alignment,
        circular=True,
    ]
    var a_smem_iter = IteratorTypeA(
        a_smem, IteratorTypeA.linear_uint_type(a_smem_size)
    )

    var b_smem = (a_smem + a_smem_size).bitcast[Scalar[a_type]]()
    comptime b_smem_size: Int = 2 * BN * BK
    comptime b_smem_layout = Layout.row_major(2 * BN, BK)
    var b_smem_full = LayoutTensor[
        a_type, b_smem_layout, MutAnyOrigin, address_space=AddressSpace.SHARED
    ](b_smem.as_unsafe_any_origin())

    # Scale staging smem (f16, double buffered): da + dsc_lo + dsc_hi.
    var scale_sm = (b_smem + b_smem_size).bitcast[Float16]()
    var da_sm = scale_sm
    var dsc_lo_sm = da_sm + 2 * num_k_mmas * BM
    var dsc_hi_sm = dsc_lo_sm + 2 * num_k_mmas * BN

    var a_gmem_iter = a.tiled_iterator[BM, BK, axis=1](block_idx.y, 0)

    comptime mma_shape = get_mma_shape[a_type, DType.int32]()
    comptime MMA_M = mma_shape[0]
    comptime MMA_N = mma_shape[1]
    comptime num_m_mmas = WM // MMA_M
    comptime num_n_mmas = WN // MMA_N
    comptime c_reg_layout = Layout.row_major(num_m_mmas * num_n_mmas, 4)
    var acc = (
        LayoutTensor[
            DType.float32,
            c_reg_layout,
            MutAnyOrigin,
            address_space=AddressSpace.LOCAL,
        ]
        .stack_allocation()
        .fill(0)
    )

    var block_m_base = Int(block_idx.y) * BM
    var block_n_base = Int(block_idx.x) * BN

    multistage_mma_q6k_native[
        BM,
        BN,
        BK,
        WM,
        WN,
        num_threads,
        num_pipeline_stages,
    ](
        acc,
        a_gmem_iter,
        a_smem_iter,
        b_smem_full,
        uceildiv(K, BK),
        w,
        rowbytes,
        da,
        da_sm.as_unsafe_any_origin(),
        dsc_lo_sm.as_unsafe_any_origin(),
        dsc_hi_sm.as_unsafe_any_origin(),
        M,
        N,
        K,
        block_m_base,
        block_n_base,
    )

    # Epilogue: cast the f32 accumulator to f16 and store into y[m_real, N]
    # row-major. Zero-padded rows (m_real..M) are computed but never stored.
    var warp_y = ufloordiv(warp_id, num_warps_n)
    var warp_x = umod(warp_id, num_warps_n)
    var lane = lane_id()
    var group_id = Int(lane) // 4
    var t_grp = Int(lane) % 4
    var warp_row_base = block_m_base + Int(warp_y) * WM
    var warp_col_base = block_n_base + Int(warp_x) * WN
    comptime for m_mma in range(num_m_mmas):
        comptime for n_mma in range(num_n_mmas):
            comptime idx = n_mma * num_m_mmas + m_mma
            comptime for e in range(4):
                comptime h = e // 2
                comptime wv = e % 2
                var t = warp_row_base + m_mma * MMA_M + group_id + 8 * h
                var col = warp_col_base + n_mma * MMA_N + t_grp * 2 + wv
                if t < m_real and col < N:
                    var v = rebind[Scalar[DType.float32]](acc[idx, e])
                    y[t * N + col] = v.cast[DType.float16]()
