# =============================================================================
# Plik: test_nvfp4_ct_bm16_golden.mojo
# Opis: Porównuje produkcyjne wrappery BM16 z referencją na realnych wagach.
# Przykład: mojo test_nvfp4_ct_bm16_golden.mojo
# =============================================================================

from std.gpu.host import DeviceBuffer, DeviceContext
from std.math import sqrt
from std.python import Python
from std.time import perf_counter_ns
from src.nvfp4_batch import (
    gemv_batch_nvfp4_f16_b4,
    gemv_batch_nvfp4_f16_b8,
    gemv_batch_nvfp4_f16_b16,
)
from src.nvfp4_ct_direct import (
    gemm_nvfp4_ct_bm16_gateup_m4,
    gemm_nvfp4_ct_bm16_gateup_m8,
    gemm_nvfp4_ct_bm16_gateup_m16,
    gemm_nvfp4_ct_bm16_o_m4,
    gemm_nvfp4_ct_bm16_o_m8,
    gemm_nvfp4_ct_bm16_o_m16,
    gemm_nvfp4_ct_bm16_qkv_m4,
    gemm_nvfp4_ct_bm16_qkv_m8,
    gemm_nvfp4_ct_bm16_qkv_m16,
    reduce_nvfp4_gate_split,
)
from src.nvfp4_ct_layout import repack_nvfp4_ct_s0_n64k128_into

comptime COLS = 4096
comptime SOURCE_ROWS = 11264
comptime CANARY = 128
comptime INV_GLOBAL_SCALE = 1.0 / 11648.0
comptime WARMUP = 20
comptime ITERS = 100
comptime ROUNDS = 7
comptime DATA = (
    "/home/critix/.cache/tentaflow-profiles/"
    "nvfp4-marlin-v2-study/v2/data/gate/"
)


def _candidate_index[rows: Int](token: Int, row: Int) -> Int:
    comptime if rows == 6144:
        comptime q_rows = 4096
        comptime kv_rows = 1024
        if row < q_rows:
            return token * q_rows + row
        if row < q_rows + kv_rows:
            return 16 * q_rows + token * kv_rows + row - q_rows
        return (
            16 * (q_rows + kv_rows)
            + token * kv_rows
            + row
            - q_rows
            - kv_rows
        )
    else:
        comptime if rows == 22528:
            comptime segment_rows = rows // 2
            if row < segment_rows:
                return token * segment_rows + row
            return 16 * segment_rows + token * segment_rows + row - segment_rows
        else:
            return token * rows + row


def _load_pattern[
    dtype: DType,
    target_bytes: Int,
    chunk_bytes: Int,
](mut buffer: DeviceBuffer[dtype], path: String) raises:
    builtins = Python.import_module("builtins")
    ctypes = Python.import_module("ctypes")
    data = builtins.open(path, "rb").read()
    with buffer.map_to_host() as target:
        var offset = 0
        while offset < target_bytes:
            count = min(chunk_bytes, target_bytes - offset)
            _ = ctypes.memmove(
                Int(target.unsafe_ptr()) + offset,
                data,
                count,
            )
            offset += count


def _candidate[rows: Int, split_k: Int, logical_m: Int](
    ctx: DeviceContext,
    mut output: DeviceBuffer[DType.float16],
    mut workspace: DeviceBuffer[DType.float16],
    mut resident: DeviceBuffer[DType.uint8],
    mut x: DeviceBuffer[DType.float16],
) raises:
    comptime if split_k == 1:
        comptime if logical_m == 4:
            ctx.enqueue_function[gemm_nvfp4_ct_bm16_gateup_m4](
                output.unsafe_ptr(), resident.unsafe_ptr(), x.unsafe_ptr(),
                Float32(INV_GLOBAL_SCALE),
                grid_dim=(rows // 128,), block_dim=256,
            )
        elif logical_m == 8:
            ctx.enqueue_function[gemm_nvfp4_ct_bm16_gateup_m8](
                output.unsafe_ptr(), resident.unsafe_ptr(), x.unsafe_ptr(),
                Float32(INV_GLOBAL_SCALE),
                grid_dim=(rows // 128,), block_dim=256,
            )
        else:
            ctx.enqueue_function[gemm_nvfp4_ct_bm16_gateup_m16](
                output.unsafe_ptr(), resident.unsafe_ptr(), x.unsafe_ptr(),
                Float32(INV_GLOBAL_SCALE),
                grid_dim=(rows // 128,), block_dim=256,
            )
    else:
        comptime if rows == 4096:
            comptime if logical_m == 4:
                ctx.enqueue_function[gemm_nvfp4_ct_bm16_o_m4](
                    workspace.unsafe_ptr(), resident.unsafe_ptr(), x.unsafe_ptr(),
                    Float32(INV_GLOBAL_SCALE),
                    grid_dim=(rows // 128 * split_k,), block_dim=256,
                )
            elif logical_m == 8:
                ctx.enqueue_function[gemm_nvfp4_ct_bm16_o_m8](
                    workspace.unsafe_ptr(), resident.unsafe_ptr(), x.unsafe_ptr(),
                    Float32(INV_GLOBAL_SCALE),
                    grid_dim=(rows // 128 * split_k,), block_dim=256,
                )
            else:
                ctx.enqueue_function[gemm_nvfp4_ct_bm16_o_m16](
                    workspace.unsafe_ptr(), resident.unsafe_ptr(), x.unsafe_ptr(),
                    Float32(INV_GLOBAL_SCALE),
                    grid_dim=(rows // 128 * split_k,), block_dim=256,
                )
        else:
            comptime if logical_m == 4:
                ctx.enqueue_function[gemm_nvfp4_ct_bm16_qkv_m4](
                    workspace.unsafe_ptr(), resident.unsafe_ptr(), x.unsafe_ptr(),
                    Float32(INV_GLOBAL_SCALE),
                    grid_dim=(rows // 128 * split_k,), block_dim=256,
                )
            elif logical_m == 8:
                ctx.enqueue_function[gemm_nvfp4_ct_bm16_qkv_m8](
                    workspace.unsafe_ptr(), resident.unsafe_ptr(), x.unsafe_ptr(),
                    Float32(INV_GLOBAL_SCALE),
                    grid_dim=(rows // 128 * split_k,), block_dim=256,
                )
            else:
                ctx.enqueue_function[gemm_nvfp4_ct_bm16_qkv_m16](
                    workspace.unsafe_ptr(), resident.unsafe_ptr(), x.unsafe_ptr(),
                    Float32(INV_GLOBAL_SCALE),
                    grid_dim=(rows // 128 * split_k,), block_dim=256,
                )
        ctx.enqueue_function[reduce_nvfp4_gate_split](
            output.unsafe_ptr(), workspace.unsafe_ptr(),
            rows, 16, split_k,
            grid_dim=((rows * 16 + 255) // 256,), block_dim=256,
        )


def _native[rows: Int, logical_m: Int](
    ctx: DeviceContext,
    mut output: DeviceBuffer[DType.float16],
    mut packed: DeviceBuffer[DType.uint8],
    mut scales: DeviceBuffer[DType.uint8],
    mut x: DeviceBuffer[DType.float16],
) raises:
    comptime if logical_m == 4:
        ctx.enqueue_function[gemv_batch_nvfp4_f16_b4](
            output.unsafe_ptr(), packed.unsafe_ptr(), scales.unsafe_ptr(),
            x.unsafe_ptr(), COLS, rows, logical_m, Float32(INV_GLOBAL_SCALE),
            grid_dim=(rows // 8,), block_dim=256,
        )
    elif logical_m == 8:
        ctx.enqueue_function[gemv_batch_nvfp4_f16_b8](
            output.unsafe_ptr(), packed.unsafe_ptr(), scales.unsafe_ptr(),
            x.unsafe_ptr(), COLS, rows, logical_m, Float32(INV_GLOBAL_SCALE),
            grid_dim=(rows // 8,), block_dim=256,
        )
    else:
        ctx.enqueue_function[gemv_batch_nvfp4_f16_b16](
            output.unsafe_ptr(), packed.unsafe_ptr(), scales.unsafe_ptr(),
            x.unsafe_ptr(), COLS, rows, logical_m, Float32(INV_GLOBAL_SCALE),
            grid_dim=(rows // 8,), block_dim=256,
        )


def _bench[rows: Int, source_rows: Int, split_k: Int, logical_m: Int]() raises:
    comptime packed_bytes = rows * COLS // 2
    comptime scale_bytes = rows * COLS // 16
    comptime source_packed_bytes = source_rows * COLS // 2
    comptime source_scale_bytes = source_rows * COLS // 16
    var ctx = DeviceContext()
    var packed = ctx.enqueue_create_buffer[DType.uint8](packed_bytes)
    var scales = ctx.enqueue_create_buffer[DType.uint8](scale_bytes)
    var resident = ctx.enqueue_create_buffer[DType.uint8](
        packed_bytes + scale_bytes
    )
    var x = ctx.enqueue_create_buffer[DType.float16](16 * COLS + CANARY)
    var native_output = ctx.enqueue_create_buffer[DType.float16](logical_m * rows)
    var output = ctx.enqueue_create_buffer[DType.float16](
        16 * rows + CANARY
    )
    var workspace = ctx.enqueue_create_buffer[DType.float16](
        2 * split_k * 16 * rows
    )
    _load_pattern[DType.uint8, packed_bytes, source_packed_bytes](
        packed, DATA + "weight_packed.bin"
    )
    _load_pattern[DType.uint8, scale_bytes, source_scale_bytes](
        scales, DATA + "weight_scale.bin"
    )
    with x.map_to_host() as values:
        for i in range(logical_m * COLS):
            values[i] = Float16(
                Float32((i * 17 + (i // COLS) * 13) % 127 - 63) * 0.00390625
            )
        for i in range(logical_m * COLS, 16 * COLS):
            values[i] = Float16(91.0)
        for i in range(16 * COLS, len(values)):
            values[i] = Float16(-77.0)
    with output.map_to_host() as values:
        for i in range(len(values)):
            values[i] = Float16(-91.0)
    ctx.enqueue_function[repack_nvfp4_ct_s0_n64k128_into](
        resident.unsafe_ptr(), packed.unsafe_ptr(), scales.unsafe_ptr(),
        COLS, rows, 0,
        grid_dim=(rows // 64 * (COLS // 128),), block_dim=128,
    )
    _native[rows, logical_m](ctx, native_output, packed, scales, x)
    _candidate[rows, split_k, logical_m](ctx, output, workspace, resident, x)
    ctx.synchronize()

    var sum_sq = 0.0
    var diff_sq = 0.0
    var max_abs = 0.0
    var top1_equal = 0
    with native_output.map_to_host() as expected, output.map_to_host() as actual:
        for token in range(logical_m):
            var expected_top = 0
            var actual_top = 0
            for row in range(rows):
                i = token * rows + row
                candidate_i = _candidate_index[rows](token, row)
                left = Float64(expected[i])
                right = Float64(actual[candidate_i])
                diff = abs(left - right)
                sum_sq += left * left
                diff_sq += diff * diff
                max_abs = max(max_abs, diff)
                if expected[i] > expected[token * rows + expected_top]:
                    expected_top = row
                if actual[candidate_i] > actual[
                    _candidate_index[rows](token, actual_top)
                ]:
                    actual_top = row
            if expected_top == actual_top:
                top1_equal += 1
        for token in range(logical_m, 16):
            for row in range(rows):
                i = _candidate_index[rows](token, row)
                if actual[i] != Float16(0.0):
                    print(
                        "tail_failure", "m", logical_m, "rows", rows,
                        "index", i, "value", actual[i],
                    )
                    raise Error("BM16 nie wyzerował fizycznego ogona")
        for i in range(16 * rows, 16 * rows + CANARY):
            if actual[i] != Float16(-91.0):
                raise Error("BM16 zapisał za fizycznym wyjściem")

    for _ in range(WARMUP):
        _native[rows, logical_m](ctx, native_output, packed, scales, x)
        _candidate[rows, split_k, logical_m](ctx, output, workspace, resident, x)
    ctx.synchronize()
    var native_times = InlineArray[Float64, ROUNDS](fill=0.0)
    var candidate_times = InlineArray[Float64, ROUNDS](fill=0.0)
    for round in range(ROUNDS):
        started = perf_counter_ns()
        for _ in range(ITERS):
            _native[rows, logical_m](ctx, native_output, packed, scales, x)
        ctx.synchronize()
        native_times[round] = (
            Float64(perf_counter_ns() - started) / 1e3 / ITERS
        )
        started = perf_counter_ns()
        for _ in range(ITERS):
            _candidate[rows, split_k, logical_m](
                ctx, output, workspace, resident, x
            )
        ctx.synchronize()
        candidate_times[round] = (
            Float64(perf_counter_ns() - started) / 1e3 / ITERS
        )
    for i in range(ROUNDS):
        for j in range(i + 1, ROUNDS):
            if native_times[j] < native_times[i]:
                native_times[i], native_times[j] = native_times[j], native_times[i]
            if candidate_times[j] < candidate_times[i]:
                candidate_times[i], candidate_times[j] = (
                    candidate_times[j], candidate_times[i]
                )
    print(
        "m", logical_m, "rows", rows, "split_k", split_k,
        "native_us", native_times[ROUNDS // 2],
        "candidate_us", candidate_times[ROUNDS // 2],
        "rel_l2", sqrt(diff_sq / sum_sq),
        "max_abs", max_abs, "top1", top1_equal, "/", logical_m,
        "canary", True,
    )


def main() raises:
    _bench[4096, 4096, 4, 4]()
    _bench[6144, 6144, 3, 4]()
    _bench[22528, SOURCE_ROWS, 1, 4]()
    _bench[4096, 4096, 4, 8]()
    _bench[6144, 6144, 3, 8]()
    _bench[22528, SOURCE_ROWS, 1, 8]()
    _bench[4096, 4096, 4, 16]()
    _bench[6144, 6144, 3, 16]()
    _bench[22528, SOURCE_ROWS, 1, 16]()
