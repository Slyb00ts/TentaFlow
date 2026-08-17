# =============================================================================
# Plik: nvfp4_ct_prefill.mojo
# Opis: Tensor-core GEMM F16 czytający wagi bezpośrednio z naturalnego układu S0.
# Przykład: gemm_nvfp4_ct_s0_f16_bm128 liczy projekcję prefill dla okna wierszy.
# =============================================================================

from std.gpu import block_idx, thread_idx
from std.gpu.sync import barrier
from std.gpu.memory import AddressSpace, async_copy_commit_group, async_copy_wait_group
from std.memory import stack_allocation
from std.gpu.compute.mma import mma, ld_matrix
from src.gemv2 import _e2m1x8
from src.gemm import _issue_x, _store_tile
from src.nvfp4_ct_layout import (
    NVFP4_CT_SCALE_BYTES,
    NVFP4_CT_TILE_BYTES,
    NVFP4_CT_TILE_COLS,
    NVFP4_CT_TILE_ROWS,
    nvfp4_ct_decode_s0,
)

comptime WARP = 32
comptime BN = 64
comptime BK = 32
comptime LDK = 40
comptime LDW = 40
comptime WTILE = BN * LDW


def gemm_nvfp4_ct_s0_impl[BM: Int, NW: Int](
    y: UnsafePointer[Float16, MutAnyOrigin],
    weights: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
    source_row_offset: Int,
    inv_global_scale: Float32,
):
    """Zachowuje arytmetykę GEMM row-major, zmieniając wyłącznie odczyt wag."""
    comptime XTILE = BM * LDK
    comptime NT = NW * WARP
    comptime x_rpp = NT // 4
    comptime w_passes = (BN * 4) // NT
    comptime m_warps = BM // 32

    tid = Int(thread_idx.x)
    lane = tid % WARP
    wid = tid // WARP
    row0 = Int(block_idx.x) * BN
    t0 = Int(block_idx.y) * BM

    xs = stack_allocation[2 * XTILE, Float16, address_space = AddressSpace.SHARED]()
    ws = stack_allocation[2 * WTILE, Float16, address_space = AddressSpace.SHARED]()

    xrow = tid // 4
    kc8 = (tid % 4) * 8
    var xr0 = t0 + xrow
    if xr0 > n_tokens - 1:
        xr0 = n_tokens - 1
    var xr1 = t0 + xrow + x_rpp
    if xr1 > n_tokens - 1:
        xr1 = n_tokens - 1
    xsrc0 = (x + xr0 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xsrc1 = (x + xr1 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xdst0 = xs + xrow * LDK + kc8
    xdst1 = xdst0 + x_rpp * LDK

    part = tid % 4
    groups = n_cols // 16
    stages_s0 = n_cols // NVFP4_CT_TILE_COLS
    var scale_base = InlineArray[
        UnsafePointer[UInt8, MutAnyOrigin], w_passes
    ](uninitialized=True)
    var code_base = InlineArray[
        UnsafePointer[UInt8, MutAnyOrigin], w_passes
    ](uninitialized=True)
    comptime for wp in range(w_passes):
        var wrow = row0 + tid // 4 + wp * (NT // 4)
        if wrow > n_rows - 1:
            wrow = n_rows - 1
        physical_row = source_row_offset + wrow
        row_tile = physical_row // NVFP4_CT_TILE_ROWS
        tile_row = physical_row % NVFP4_CT_TILE_ROWS
        row_tile_base = (
            weights + row_tile * stages_s0 * NVFP4_CT_TILE_BYTES
        )
        scale_base[wp] = row_tile_base + tile_row * 8
        code_base[wp] = (
            row_tile_base
            + NVFP4_CT_SCALE_BYTES
            + tile_row * 64
            + (part % 2) * 4
        )
    wdst = ws + (tid // 4) * LDW + part * 8

    group = lane >> 2
    tid4 = lane & 3
    warp_m = (wid % m_warps) * 32
    warp_n = (wid // m_warps) * 32
    sub = lane // 8
    lr = lane % 8
    a_base = xs + (warp_m + (sub % 2) * 8 + lr) * LDK + (sub // 2) * 8
    b_base = ws + (warp_n + lr) * LDW + (sub % 2) * 8

    var acc = InlineArray[SIMD[DType.float32, 4], 8](
        fill=SIMD[DType.float32, 4](0.0)
    )
    n_stages = (n_cols + BK - 1) // BK
    g_mine = part // 2

    def fetch_w(
        s: Int,
        groups: Int,
        g_mine: Int,
        inv_global_scale: Float32,
        scale_base: InlineArray[
            UnsafePointer[UInt8, MutAnyOrigin], w_passes
        ],
        code_base: InlineArray[
            UnsafePointer[UInt8, MutAnyOrigin], w_passes
        ],
        mut qv: InlineArray[SIMD[DType.uint8, 4], w_passes],
        mut sc: InlineArray[Float32, w_passes],
    ):
        group_abs = s * 2 + g_mine
        tile_offset = (s // 4) * NVFP4_CT_TILE_BYTES
        group_in_tile = (s % 4) * 2 + g_mine

        comptime for wp in range(w_passes):
            if group_abs < groups:
                qv[wp] = (
                    code_base[wp]
                    + tile_offset
                    + group_in_tile * 8
                ).load[width=4, alignment=4]()
                encoded_scale = scale_base[wp][
                    tile_offset + group_in_tile
                ]
                sc[wp] = Float32(nvfp4_ct_decode_s0(encoded_scale)) * (
                    inv_global_scale / 128.0
                )
            else:
                qv[wp] = SIMD[DType.uint8, 4](0)
                sc[wp] = 0.0

    _issue_x[BM, NW](0, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
    async_copy_commit_group()
    var qv = InlineArray[SIMD[DType.uint8, 4], w_passes](
        fill=SIMD[DType.uint8, 4](0)
    )
    var sc = InlineArray[Float32, w_passes](fill=0.0)
    fetch_w(
        0,
        groups,
        g_mine,
        inv_global_scale,
        scale_base,
        code_base,
        qv,
        sc,
    )

    var s = 0
    while s < n_stages:
        if s + 1 < n_stages:
            _issue_x[BM, NW](s + 1, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
            async_copy_commit_group()

        comptime for wp in range(w_passes):
            lo = qv[wp] & 0x0F
            hi = qv[wp] >> 4
            codes = lo.interleave(hi)
            wv = (_e2m1x8(codes) * sc[wp]).cast[DType.float16]()
            (wdst + (s % 2) * WTILE + wp * (NT // 4) * LDW).store[
                width=8, alignment=16
            ](wv)
        if s + 1 < n_stages:
            fetch_w(
                s + 1,
                groups,
                g_mine,
                inv_global_scale,
                scale_base,
                code_base,
                qv,
                sc,
            )
            async_copy_wait_group(1)
        else:
            async_copy_wait_group(0)
        barrier()

        buf = s % 2
        comptime for k16 in range(BK // 16):
            comptime kb = k16 * 16
            a0 = ld_matrix[8](a_base + buf * XTILE + kb)
            a1 = ld_matrix[8](a_base + buf * XTILE + kb + 16 * LDK)
            comptime for ni in range(4):
                b = ld_matrix[4](b_base + buf * WTILE + ni * 8 * LDW + kb)
                mma(acc[ni], a0, b, acc[ni])
                mma(acc[4 + ni], a1, b, acc[4 + ni])
        barrier()
        s += 1

    _store_tile(
        y, acc, t0, row0, warp_m, warp_n, group, tid4, n_rows, n_tokens
    )


comptime gemm_nvfp4_ct_s0_f16_bm128 = gemm_nvfp4_ct_s0_impl[128, 8]
comptime gemm_nvfp4_ct_s0_f16_bm64 = gemm_nvfp4_ct_s0_impl[64, 4]
