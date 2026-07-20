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
# PACKED Q4_K int8 multistage GEMM — reproduces MMQ's packed-weight data path.
# Derived from the per-32-block Q4_K kernel (multistage_i8_q4k.mojo, Finding M).
# Removes the two bottlenecks Finding M measured:
#   1. Weights are read from HBM as PACKED 4-bit nibbles ([N, K/2] uint8) — HALF
#      the bandwidth of the unpacked int8 [N,K] matrix. The packed bytes are
#      cp.async'd into a pipelined smem staging buffer (num_pipeline_stages deep,
#      exactly like the activation), then unpacked in-kernel into a double-
#      buffered plain int8 smem tile that the s8 mma consumes (b_swizzle=False,
#      so the unpack is a straight row-major write — no ldmatrix swizzle to
#      reproduce). The unpack reads staged smem (fast), NOT global memory, so the
#      HBM latency is hidden by the pipeline.
#   2. Scales are f16 (half2), matching MMQ's block scale storage — halves the
#      scale-tensor HBM traffic.
#
# The activation stays q8_1 (int8 codes + per-32 f16 d_a / s_a). The per-32-block
# flush is bit-identical to vec_dot_q4_K_q8_1_impl_mmq, so quality == Q4_K MMQ
# by construction.

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
def multistage_mma_q4k_pack[
    a_type: DType,
    a_layout: Layout,
    a_smem_layout: Layout,
    bp_layout: Layout,
    bp_smem_layout: Layout,
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
    bp_iter_arg: LayoutTensorIter[_, bp_layout, ...],
    bp_smem_iter_arg: LayoutTensorIter[
        mut=True, DType.uint8, bp_smem_layout, address_space=AddressSpace.SHARED, ...
    ],
    b_smem_full: LayoutTensor[
        mut=True, a_type, b_smem_layout, address_space=AddressSpace.SHARED, ...
    ],
    num_iters: Int,
    # per-sub-block weight scale tensors (global memory, f16).
    dsc: UnsafePointer[Float16, ImmutAnyOrigin],
    dm: UnsafePointer[Float16, ImmutAnyOrigin],
    # per-sub-block activation scales (f32, from quantize_act_q8_1); staged into
    # smem as f16 so the per-block flush reproduces the proven f16-scale path.
    da: UnsafePointer[Float32, ImmutAnyOrigin],
    sa: UnsafePointer[Float32, ImmutAnyOrigin],
    # per-k-tile scale scratch in shared memory (num_k_mmas sub-blocks each).
    da_sm: UnsafePointer[
        Float16, MutAnyOrigin, address_space = AddressSpace.SHARED
    ],
    sa_sm: UnsafePointer[
        Float16, MutAnyOrigin, address_space = AddressSpace.SHARED
    ],
    dsc_sm: UnsafePointer[
        Float16, MutAnyOrigin, address_space = AddressSpace.SHARED
    ],
    dm_sm: UnsafePointer[
        Float16, MutAnyOrigin, address_space = AddressSpace.SHARED
    ],
    M: Int,
    N: Int,
    K: Int,
    nkb: Int,
    block_m_base: Int,
    block_n_base: Int,
):
    comptime simd_size = simd_width_of[a_type]()
    comptime pbytes = BK // 2
    comptime simd_bp = 16
    comptime transpose_b = True

    var full_tid = thread_idx.x
    var tid = UInt32(umod(thread_idx.x, num_threads))
    var warp_id = tid // UInt32(WARP_SIZE)
    var lane = lane_id()

    comptime num_warps_n = BN // WN
    var warp_y, warp_x = divmod(warp_id, UInt32(num_warps_n))

    var a_iter = a_iter_arg
    var a_smem_iter = a_smem_iter_arg
    var bp_iter = bp_iter_arg
    var bp_smem_iter = bp_smem_iter_arg

    comptime a_num_vecs = BM * BK // simd_size
    comptime async_copy_a_layout = Layout.row_major(
        min(num_threads, a_num_vecs) * simd_size // BK, BK // simd_size
    )
    comptime bp_num_vecs = BN * pbytes // simd_bp
    comptime async_copy_bp_layout = Layout.row_major(
        min(num_threads, bp_num_vecs) * simd_bp // pbytes, pbytes // simd_bp
    )

    @always_inline
    @parameter
    def _copy_a_to_sram(dst: LayoutTensor[mut=True, ...], src: LayoutTensor):
        copy_dram_to_sram_async[
            thread_layout=async_copy_a_layout,
            swizzle=True,
            num_threads=num_threads,
        ](dst.vectorize[1, simd_size](), src.vectorize[1, simd_size]())

    @always_inline
    @parameter
    def _copy_bp_to_sram(dst: LayoutTensor[mut=True, ...], src: LayoutTensor):
        copy_dram_to_sram_async[
            thread_layout=async_copy_bp_layout,
            swizzle=False,
            num_threads=num_threads,
        ](dst.vectorize[1, simd_bp](), src.vectorize[1, simd_bp]())

    var b_base = b_smem_full.ptr

    @always_inline
    @parameter
    def _unpack_b(
        bp_sm: UnsafePointer[
            UInt8, MutAnyOrigin, address_space = AddressSpace.SHARED
        ],
        buf: Int,
    ):
        # Unpack this k-tile's staged packed nibbles ([BN, BK/2] bytes in smem)
        # into the plain int8 buffer. b_swizzle=False, so plain row-major write.
        var buf_off = buf * BN * BK
        var i = Int(full_tid)
        while i < BN * pbytes:
            var n = i // pbytes
            var bc = i % pbytes
            var byte = Int(bp_sm[n * pbytes + bc])
            # b_swizzle=False on the mma load, so write plain row-major (no
            # ldmatrix swizzle to reproduce).
            var o = buf_off + n * BK + 2 * bc
            b_base[o] = Scalar[a_type](byte & 0xF)
            b_base[o + 1] = Scalar[a_type]((byte >> 4) & 0xF)
            i += num_threads

    # Prefetch (num_pipeline_stages - 1) A + packed-B stages.
    comptime for stage in range(num_pipeline_stages - 1):
        var a_smem_tile = a_smem_iter.next_unsafe(
            a_smem_iter.linear_uint_type(stage)
        )[]
        _copy_a_to_sram(a_smem_tile, a_iter[])
        a_iter._incr()
        var bp_smem_tile = bp_smem_iter.next_unsafe(
            bp_smem_iter.linear_uint_type(stage)
        )[]
        _copy_bp_to_sram(bp_smem_tile, bp_iter[])
        bp_iter._incr()
        async_copy_commit_group()

    async_copy_wait_group(Int32(num_pipeline_stages - 2))
    barrier()

    # Unpack B for k-tile 0 into buffer 0 (bp_smem_iter[] points at stage 0).
    _unpack_b(bp_smem_iter[].ptr.as_unsafe_any_origin(), 0)
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

    var c_tmp = LayoutTensor[
        DType.int32,
        Layout.row_major(num_m_mmas * num_n_mmas, c_frag_size),
        MutAnyOrigin,
        address_space=AddressSpace.LOCAL,
    ].stack_allocation()

    comptime swizzle_a_pattern = make_ldmatrix_swizzle[
        a_type, a_warp_tile.stride[0]()
    ]()

    var groupID = Int(lane) // 4
    var tgrp = Int(lane) % 4
    var warp_lrow = Int(warp_y) * WM
    var warp_lcol = Int(warp_x) * WN

    # Load k=0 fragment (sub-block 0 of k-tile 0) from B buffer 0.
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
        var i = Int(full_tid)
        while i < row_stride:
            var sub = i // BM
            var lr = i % BM
            var kb = k_tile_id * num_k_mmas + sub
            var g = kb * M + block_m_base + lr
            da_sm[ro + i] = Float16(da[g])
            sa_sm[ro + i] = Float16(sa[g])
            i += num_threads
        var j = Int(full_tid)
        while j < col_stride:
            var sub = j // BN
            var lc = j % BN
            var kb = k_tile_id * num_k_mmas + sub
            var g = kb * N + block_n_base + lc
            dsc_sm[co + j] = dsc[g]
            dm_sm[co + j] = dm[g]
            j += num_threads

    @always_inline
    @parameter
    def _flush(sub: Int, cur: Int, buf: Int):
        var ro = buf * row_stride + sub * BM
        var co = buf * col_stride + sub * BN
        var da_r = InlineArray[Float32, num_m_mmas * 2](fill=0)
        var sa_r = InlineArray[Float32, num_m_mmas * 2](fill=0)
        comptime for m_mma in range(num_m_mmas):
            comptime for h in range(2):
                var lr = warp_lrow + m_mma * MMA_M + groupID + 8 * h
                da_r[m_mma * 2 + h] = Float32(da_sm[ro + lr])
                sa_r[m_mma * 2 + h] = Float32(sa_sm[ro + lr])
        var dsc_r = InlineArray[Float32, num_n_mmas * 2](fill=0)
        var dm_r = InlineArray[Float32, num_n_mmas * 2](fill=0)
        comptime for n_mma in range(num_n_mmas):
            comptime for w in range(2):
                var lc = warp_lcol + n_mma * MMA_N + tgrp * 2 + w
                dsc_r[n_mma * 2 + w] = Float32(dsc_sm[co + lc])
                dm_r[n_mma * 2 + w] = Float32(dm_sm[co + lc])

        _ = c_tmp.fill(0)
        mma_op.mma(
            a_reg_tiles[cur].vectorize[1, a_frag_size](),
            b_reg_tiles[cur],
            c_tmp.vectorize[1, c_frag_size](),
        )
        comptime for m_mma in range(num_m_mmas):
            comptime for n_mma in range(num_n_mmas):
                comptime idx = n_mma * num_m_mmas + m_mma
                comptime for e in range(4):
                    comptime h = e // 2
                    comptime w = e % 2
                    acc[idx, e] += (
                        dsc_r[n_mma * 2 + w]
                        * da_r[m_mma * 2 + h]
                        * Float32(Int(c_tmp[idx, e]))
                        - dm_r[n_mma * 2 + w] * sa_r[m_mma * 2 + h]
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
                    var bp_pf = bp_smem_iter.next_unsafe(
                        bp_smem_iter.linear_uint_type(num_pipeline_stages - 1)
                    )[]
                    _copy_bp_to_sram(bp_pf, bp_iter[])
                    bp_iter._incr()
                async_copy_commit_group()
                async_copy_wait_group(Int32(num_pipeline_stages - 2))

                a_smem_iter._incr()
                bp_smem_iter._incr()
                # Unpack the newly-current packed stage into the other buffer.
                if k_tile_id + 1 < num_iters:
                    _unpack_b(bp_smem_iter[].ptr.as_unsafe_any_origin(), (k_tile_id + 1) % 2)
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


@__name(t"multistage_gemm_q4k_pack_kernel_{a_type}")
def multistage_gemm_q4k_pack_kernel[
    CLT: TensorLayout,
    a_type: DType,
    ALT: TensorLayout,
    BPLT: TensorLayout,
    c_linear_idx_type: DType,
    a_linear_idx_type: DType,
    bp_linear_idx_type: DType,
    config: MatmulConfig[a_type, a_type, DType.float32, True, ...],
](
    c_tt: TileTensor[
        DType.float32, CLT, MutAnyOrigin, linear_idx_type=c_linear_idx_type
    ],
    a_tt: TileTensor[
        a_type, ALT, ImmutAnyOrigin, linear_idx_type=a_linear_idx_type
    ],
    bp_tt: TileTensor[
        DType.uint8, BPLT, ImmutAnyOrigin, linear_idx_type=bp_linear_idx_type
    ],
    dsc: UnsafePointer[Float16, ImmutAnyOrigin],
    dm: UnsafePointer[Float16, ImmutAnyOrigin],
    da: UnsafePointer[Float32, ImmutAnyOrigin],
    sa: UnsafePointer[Float32, ImmutAnyOrigin],
    y: UnsafePointer[Float16, MutAnyOrigin],
    m_real: Int,
):
    var c = c_tt.to_layout_tensor()
    var a = a_tt.to_layout_tensor()
    var bp = bp_tt.to_layout_tensor()

    comptime assert a_type == DType.int8, "q4k pipeline only supports S8 mma"

    var M: Int = c.dim[0]()
    var N: Int = c.dim[1]()
    var K: Int = a.dim[1]()
    var nkb = K // 32

    comptime BM = config.block_tile_shape[0]
    comptime BN = config.block_tile_shape[1]
    comptime BK = config.block_tile_shape[2]
    comptime WM = config.warp_tile_shape[0]
    comptime WN = config.warp_tile_shape[1]
    comptime num_pipeline_stages = config.num_pipeline_stages
    comptime num_warps_n = config.num_warps_n()
    comptime num_threads = config.num_threads()
    comptime num_k_mmas = BK // 32
    comptime pbytes = BK // 2

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

    # Packed-B staging smem (pipelined, uint8 [BN, BK/2] per stage).
    var bp_smem = (a_smem + a_smem_size).bitcast[UInt8]()
    comptime bp_smem_size: Int = num_pipeline_stages * BN * pbytes
    comptime IteratorTypeBP = LayoutTensorIter[
        DType.uint8,
        Layout.row_major(BN, pbytes),
        MutAnyOrigin,
        address_space=AddressSpace.SHARED,
        circular=True,
    ]
    var bp_smem_iter = IteratorTypeBP(
        bp_smem.as_unsafe_any_origin(),
        IteratorTypeBP.linear_uint_type(bp_smem_size),
    )

    # B smem: double-buffered unpacked int8 [2*BN, BK].
    var b_smem = (bp_smem + bp_smem_size).bitcast[Scalar[a_type]]()
    comptime b_smem_size: Int = 2 * BN * BK
    comptime b_smem_layout = Layout.row_major(2 * BN, BK)
    var b_smem_full = LayoutTensor[
        a_type, b_smem_layout, MutAnyOrigin, address_space=AddressSpace.SHARED
    ](b_smem.as_unsafe_any_origin())

    # Scale staging smem (f16, double buffered).
    var scale_sm = (b_smem + b_smem_size).bitcast[Float16]()
    var da_sm = scale_sm
    var sa_sm = da_sm + 2 * num_k_mmas * BM
    var dsc_sm = sa_sm + 2 * num_k_mmas * BM
    var dm_sm = dsc_sm + 2 * num_k_mmas * BN

    var a_gmem_iter = a.tiled_iterator[BM, BK, axis=1](block_idx.y, 0)
    var bp_gmem_iter = bp.tiled_iterator[BN, pbytes, axis=1](block_idx.x, 0)

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

    multistage_mma_q4k_pack[
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
        bp_gmem_iter,
        bp_smem_iter,
        b_smem_full,
        uceildiv(K, BK),
        dsc,
        dm,
        da,
        sa,
        da_sm.as_unsafe_any_origin(),
        sa_sm.as_unsafe_any_origin(),
        dsc_sm.as_unsafe_any_origin(),
        dm_sm.as_unsafe_any_origin(),
        M,
        N,
        K,
        nkb,
        block_m_base,
        block_n_base,
    )

    # Epilogue: cast the f32 accumulator to f16 and store into y[m_real, N]
    # row-major (matches the model's [token, row] projection buffer). Zero-padded
    # rows (m_real..M) are computed but never stored. The m16n8 fragment→global
    # map: row = groupID + 8*h, col = 2*(lane%4) + w over the warp's WM×WN tile.
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
                comptime w = e % 2
                var t = warp_row_base + m_mma * MMA_M + group_id + 8 * h
                var col = warp_col_base + n_mma * MMA_N + t_grp * 2 + w
                if t < m_real and col < N:
                    var v = rebind[Scalar[DType.float32]](acc[idx, e])
                    y[t * N + col] = v.cast[DType.float16]()
