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
# Per-32-block Q4_K int8 multistage GEMM. Derived from Modular's multistage
# pipeline (forked in multistage_i8.mojo). BK=64 keeps the register double
# buffer; each inner m16n8k32 mma covers exactly one Q4_K 32-column sub-block,
# so its int32 dot is flushed into a persistent f32 accumulator with the
# per-sub-block Q4_K scales:
#
#   out[t,n] += d[n]*sc[n,kb]*d_a[t,kb]*sumi  -  dmin[n]*m[n,kb]*s_a[t,kb]
#
# where kb = k_tile_id*num_k_mmas + k_mma is the global 32-block index.
# Scales are pre-folded on the host into dsc = d*sc, dm = dmin*m (weight side)
# and da = d_a, sa = s_a = d_a*sum (activation q8_1 side). This mirrors
# llama.cpp `vec_dot_q4_K_q8_1_impl_mmq` exactly, so a bit-exact per-block
# kernel matches the Q4_K MMQ quality by construction.
#
# The per-k-tile scales are staged into shared memory once (cooperative load,
# BM+BN reads across the whole block), so the flush reads them at smem latency
# with no redundant per-fragment global loads — the difference between ~90 and
# ~300 TOPS.

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
def multistage_mma_q4k_f16[
    a_type: DType,
    a_layout: Layout,
    a_smem_layout: Layout,
    b_type: DType,
    b_layout: Layout,
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
    b_iter_arg: LayoutTensorIter[b_type, b_layout, ...],
    a_smem_iter_arg: LayoutTensorIter[
        mut=True, a_type, a_smem_layout, address_space=AddressSpace.SHARED, ...
    ],
    mut b_smem_iter: LayoutTensorIter[
        mut=True, b_type, b_smem_layout, address_space=AddressSpace.SHARED, ...
    ],
    num_iters: Int,
    # per-sub-block weight scale tensors (global memory, f16)
    dsc: UnsafePointer[Float16, ImmutAnyOrigin],
    dm: UnsafePointer[Float16, ImmutAnyOrigin],
    # per-sub-block activation scales (f32, from quantize_act_q8_1); staged into
    # smem as f16 so the per-block flush reproduces the proven f16-scale path.
    da: UnsafePointer[Float32, ImmutAnyOrigin],
    sa: UnsafePointer[Float32, ImmutAnyOrigin],
    # per-k-tile scale scratch in shared memory (num_k_mmas sub-blocks each)
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
    nkb: Int,
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
    var b_iter = b_iter_arg
    var a_smem_iter = a_smem_iter_arg

    comptime a_num_vecs = BM * BK // simd_size
    comptime async_copy_a_layout = Layout.row_major(
        min(num_threads, a_num_vecs) * simd_size // BK, BK // simd_size
    )
    comptime b_num_ves = BN * BK // simd_size
    comptime async_copy_b_layout = Layout.row_major(
        min(num_threads, b_num_ves)
        * simd_size
        // b_smem_layout.shape[1].value(),
        b_smem_layout.shape[1].value() // simd_size,
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
    def _copy_b_to_sram(dst: LayoutTensor[mut=True, ...], src: LayoutTensor):
        copy_dram_to_sram_async[
            thread_layout=async_copy_b_layout,
            swizzle=True,
            num_threads=num_threads,
        ](dst.vectorize[1, simd_size](), src.vectorize[1, simd_size]())

    # Prefetch (num_pipeline_stages - 1) stages.
    comptime for stage in range(num_pipeline_stages - 1):
        var a_smem_tile = a_smem_iter.next_unsafe(
            a_smem_iter.linear_uint_type(stage)
        )[]
        _copy_a_to_sram(a_smem_tile, a_iter[])
        a_iter._incr()

        var b_smem_tile = b_smem_iter.next_unsafe(
            b_smem_iter.linear_uint_type(stage)
        )[]
        _copy_b_to_sram(b_smem_tile, b_iter[])
        b_iter._incr()

        async_copy_commit_group()

    async_copy_wait_group(Int32(num_pipeline_stages - 2))
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
            b_type, b_reg_layout, MutAnyOrigin, address_space=AddressSpace.LOCAL
        ]
        .stack_allocation()
        .vectorize[1, b_frag_size]()
        .split[2]()
    )

    var a_warp_tile = a_smem_iter[].tile[WM, BK](Int(warp_y), 0)
    var b_warp_tile = b_smem_iter[].tile[WN, BK](Int(warp_x), 0)

    var mma_op = TensorCore[DType.int32, a_type, mma_shape, transpose_b]()

    var c_tmp = LayoutTensor[
        DType.int32,
        Layout.row_major(num_m_mmas * num_n_mmas, c_frag_size),
        MutAnyOrigin,
        address_space=AddressSpace.LOCAL,
    ].stack_allocation()

    comptime swizzle_a_pattern = make_ldmatrix_swizzle[
        a_type, a_warp_tile.stride[0]()
    ]()

    # Per-thread fragment output coordinates within the block tile (constant
    # across K): local row/col into the BM×BN scale smem staging buffers.
    var groupID = Int(lane) // 4
    var tgrp = Int(lane) % 4
    var warp_lrow = Int(warp_y) * WM
    var warp_lcol = Int(warp_x) * WN

    # Load k=0 fragment (sub-block 0 of k-tile 0).
    mma_op.load_a[swizzle_a_pattern](
        a_warp_tile, a_reg_tiles[0].vectorize[1, a_frag_size](), 0
    )
    mma_op.load_b(b_warp_tile, b_reg_tiles[0], 0, Int(warp_x))

    comptime row_stride = num_k_mmas * BM
    comptime col_stride = num_k_mmas * BN

    @always_inline
    @parameter
    def _stage_scales(k_tile_id: Int, buf: Int):
        # Cooperative load of this k-tile's sub-blocks of scales into smem.
        var ro = buf * row_stride
        var co = buf * col_stride
        # kb-major scale layout (da[kb*M+t], dsc[kb*N+n]) makes this cooperative
        # load fully coalesced across the BM rows / BN cols.
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
        # Hoist this thread's row/col scales from smem into registers.
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

        # All sub-block mmas back-to-back into independent int32 accumulators
        # (tensor-core ILP), then a single flush pass applies the scales.
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
        # Stage the next k-tile's scales into the other buffer; the global-load
        # latency overlaps with this tile's mma work, hidden by the end barrier.
        if k_tile_id + 1 < num_iters:
            _stage_scales(k_tile_id + 1, (k_tile_id + 1) % 2)

        var a_wt = a_smem_iter[].tile[WM, BK](Int(warp_y), 0)
        var b_wt = b_smem_iter[].tile[WN, BK](Int(warp_x), 0)

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
                    var b_pf = b_smem_iter.next_unsafe(
                        b_smem_iter.linear_uint_type(num_pipeline_stages - 1)
                    )[]
                    _copy_b_to_sram(b_pf, b_iter[])
                    b_iter._incr()
                async_copy_commit_group()
                async_copy_wait_group(Int32(num_pipeline_stages - 2))
                barrier()

                a_smem_iter._incr()
                b_smem_iter._incr()
                a_wt = a_smem_iter[].tile[WM, BK](Int(warp_y), 0)
                b_wt = b_smem_iter[].tile[WN, BK](Int(warp_x), 0)

            comptime kidx = k_mma_next % num_k_mmas
            mma_op.load_a[swizzle_a_pattern](
                a_wt, a_reg_tiles[nxt].vectorize[1, a_frag_size](), kidx
            )
            mma_op.load_b(b_wt, b_reg_tiles[nxt], kidx, Int(warp_x))

            comptime cur = k_mma % num_reg_tiles
            _flush(k_mma, cur, buf)

        # Publish the next tile's staged scales and complete this tile's reads
        # before the following iteration overwrites this buffer.
        barrier()


@__name(t"multistage_gemm_q4k_f16_kernel_{a_type}")
def multistage_gemm_q4k_f16_kernel[
    CLT: TensorLayout,
    a_type: DType,
    ALT: TensorLayout,
    BLT: TensorLayout,
    c_linear_idx_type: DType,
    a_linear_idx_type: DType,
    b_linear_idx_type: DType,
    config: MatmulConfig[a_type, a_type, DType.float32, True, ...],
](
    c_tt: TileTensor[
        DType.float32, CLT, MutAnyOrigin, linear_idx_type=c_linear_idx_type
    ],
    a_tt: TileTensor[
        a_type, ALT, ImmutAnyOrigin, linear_idx_type=a_linear_idx_type
    ],
    b_tt: TileTensor[
        a_type, BLT, ImmutAnyOrigin, linear_idx_type=b_linear_idx_type
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
    var b = b_tt.to_layout_tensor()

    comptime assert a_type == DType.int8, "q4k pipeline only supports S8 mma"

    var M: Int = c.dim[0]()
    var N: Int = b.dim[0]()
    var K: Int = b.dim[1]()
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
    comptime b_smem_size: Int = num_pipeline_stages * BK * BN
    comptime b_smem_layout = Layout.row_major(BN, BK)
    comptime IteratorTypeB = LayoutTensorIter[
        a_type,
        b_smem_layout,
        MutAnyOrigin,
        address_space=AddressSpace.SHARED,
        circular=True,
    ]
    var b_smem_iter = IteratorTypeB(
        b_smem.as_unsafe_any_origin(),
        IteratorTypeB.linear_uint_type(b_smem_size),
    )

    # Scale staging smem after the operand buffers (f32-aligned), double
    # buffered so the next tile's scales load while this tile computes.
    var scale_sm = (b_smem + b_smem_size).bitcast[Float16]()
    var da_sm = scale_sm
    var sa_sm = da_sm + 2 * num_k_mmas * BM
    var dsc_sm = sa_sm + 2 * num_k_mmas * BM
    var dm_sm = dsc_sm + 2 * num_k_mmas * BN

    var a_gmem_iter = a.tiled_iterator[BM, BK, axis=1](block_idx.y, 0)
    var b_gmem_iter = b.tiled_iterator[BN, BK, axis=1](block_idx.x, 0)

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

    multistage_mma_q4k_f16[
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
        b_gmem_iter,
        a_smem_iter,
        b_smem_iter,
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
