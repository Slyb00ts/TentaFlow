# =============================================================================
# Plik: test_nvfp4_ct_b1.mojo
# Opis: Porównuje produkcyjny repack i B1 CT z row-major na czterech wagach.
# Przykład: pixi run mojo test_nvfp4_ct_b1.mojo
# =============================================================================

from std.gpu.host import DeviceBuffer, DeviceContext
from std.math import sqrt
from std.python import Python
from std.time import perf_counter_ns
from src.gemv2 import gemv_nvfp4_f16_v2
from src.nvfp4_ct_layout import repack_nvfp4_ct_s0_n64k128_into
from src.nvfp4_ct_decode import gemv_nvfp4_ct_s0_n64k128_f16

comptime WARMUP = 20
comptime ITERS = 200
comptime ROUNDS = 7
comptime DATA_ROOT = (
    "/home/critix/.cache/tentaflow-profiles/"
    "nvfp4-marlin-v2-study/v2/data/"
)


def _load_repeated[
    size: Int, repeats: Int
](
    mut buffer: DeviceBuffer[DType.uint8],
    path: String,
) raises:
    builtins = Python.import_module("builtins")
    ctypes = Python.import_module("ctypes")
    data = builtins.open(path, "rb").read()
    if Int(data.__len__()) < size:
        raise Error("za mało danych w pliku " + path)
    with buffer.map_to_host() as target:
        for repeat in range(repeats):
            _ = ctypes.memmove(
                Int(target.unsafe_ptr()) + repeat * size,
                data,
                size,
            )


def _median(mut values: InlineArray[Float64, ROUNDS]) -> Float64:
    for i in range(ROUNDS):
        for j in range(i + 1, ROUNDS):
            if values[j] < values[i]:
                values[i], values[j] = values[j], values[i]
    return values[ROUNDS // 2]


def _native[rows: Int, cols: Int](
    ctx: DeviceContext,
    mut output: DeviceBuffer[DType.float16],
    mut packed: DeviceBuffer[DType.uint8],
    mut scales: DeviceBuffer[DType.uint8],
    mut x: DeviceBuffer[DType.float16],
    inv_global_scale: Float32,
) raises:
    ctx.enqueue_function[gemv_nvfp4_f16_v2](
        output.unsafe_ptr(), packed.unsafe_ptr(), scales.unsafe_ptr(),
        x.unsafe_ptr(), cols, rows, inv_global_scale,
        grid_dim=((rows + 7) // 8,), block_dim=256,
    )


def _candidate[rows: Int, cols: Int](
    ctx: DeviceContext,
    mut output: DeviceBuffer[DType.float16],
    mut resident: DeviceBuffer[DType.uint8],
    mut x: DeviceBuffer[DType.float16],
    inv_global_scale: Float32,
) raises:
    ctx.enqueue_function[gemv_nvfp4_ct_s0_n64k128_f16](
        output.unsafe_ptr(), resident.unsafe_ptr(), x.unsafe_ptr(),
        cols, rows, 1, 0, inv_global_scale,
        grid_dim=((rows + 7) // 8,), block_dim=256,
    )


def _shape[
    rows: Int,
    cols: Int,
    source_rows: Int,
    repeats: Int,
](
    ctx: DeviceContext,
    name: String,
    source: String,
    inv_global_scale: Float32,
) raises:
    comptime source_packed_bytes = source_rows * cols // 2
    comptime source_scale_bytes = source_rows * cols // 16
    comptime packed_bytes = rows * cols // 2
    comptime scale_bytes = rows * cols // 16
    comptime resident_bytes = packed_bytes + scale_bytes
    var packed = ctx.enqueue_create_buffer[DType.uint8](packed_bytes)
    var scales = ctx.enqueue_create_buffer[DType.uint8](scale_bytes)
    var resident_with_canary = ctx.enqueue_create_buffer[DType.uint8](
        resident_bytes + 256
    )
    var x = ctx.enqueue_create_buffer[DType.float16](cols)
    var reference = ctx.enqueue_create_buffer[DType.float16](rows)
    var candidate = ctx.enqueue_create_buffer[DType.float16](rows)
    _load_repeated[source_packed_bytes, repeats](
        packed, DATA_ROOT + source + "/weight_packed.bin"
    )
    _load_repeated[source_scale_bytes, repeats](
        scales, DATA_ROOT + source + "/weight_scale.bin"
    )
    with resident_with_canary.map_to_host() as values:
        for i in range(len(values)):
            values[i] = UInt8(0xA5)
    with x.map_to_host() as values:
        for i in range(len(values)):
            values[i] = Float16(
                Float32((i * 17 + 13) % 127 - 63) * 0.00390625
            )

    ctx.enqueue_function[repack_nvfp4_ct_s0_n64k128_into](
        resident_with_canary.unsafe_ptr(),
        packed.unsafe_ptr(),
        scales.unsafe_ptr(),
        cols,
        rows,
        0,
        grid_dim=(rows // 64 * (cols // 128),),
        block_dim=128,
    )
    _native[rows, cols](
        ctx, reference, packed, scales, x, inv_global_scale
    )
    _candidate[rows, cols](
        ctx, candidate, resident_with_canary, x, inv_global_scale
    )
    ctx.synchronize()

    var sum_sq = 0.0
    var diff_sq = 0.0
    var max_abs = 0.0
    var reference_top = 0
    var candidate_top = 0
    with reference.map_to_host() as expected, candidate.map_to_host() as actual:
        for row in range(rows):
            left = Float64(expected[row])
            right = Float64(actual[row])
            diff = abs(left - right)
            sum_sq += left * left
            diff_sq += diff * diff
            max_abs = max(max_abs, diff)
            if expected[row] > expected[reference_top]:
                reference_top = row
            if actual[row] > actual[candidate_top]:
                candidate_top = row
    var canary_ok = True
    with resident_with_canary.map_to_host() as values:
        for i in range(resident_bytes, resident_bytes + 256):
            if values[i] != UInt8(0xA5):
                canary_ok = False

    for _ in range(WARMUP):
        _native[rows, cols](
            ctx, reference, packed, scales, x, inv_global_scale
        )
        _candidate[rows, cols](
            ctx, candidate, resident_with_canary, x, inv_global_scale
        )
    ctx.synchronize()
    var native_times = InlineArray[Float64, ROUNDS](fill=0.0)
    var candidate_times = InlineArray[Float64, ROUNDS](fill=0.0)
    for round in range(ROUNDS):
        started = perf_counter_ns()
        for _ in range(ITERS):
            _native[rows, cols](
                ctx, reference, packed, scales, x, inv_global_scale
            )
        ctx.synchronize()
        native_times[round] = (
            Float64(perf_counter_ns() - started) / 1e3 / ITERS
        )
        started = perf_counter_ns()
        for _ in range(ITERS):
            _candidate[rows, cols](
                ctx, candidate, resident_with_canary, x, inv_global_scale
            )
        ctx.synchronize()
        candidate_times[round] = (
            Float64(perf_counter_ns() - started) / 1e3 / ITERS
        )
    print(
        name,
        "bytes", resident_bytes,
        "native_us", _median(native_times),
        "candidate_us", _median(candidate_times),
        "rel_l2", sqrt(diff_sq / sum_sq),
        "max_abs", max_abs,
        "top1", reference_top == candidate_top,
        "canary", canary_ok,
    )


def main() raises:
    var ctx = DeviceContext()
    _shape[6144, 4096, 6144, 1](
        ctx, "qkv", "gate", Float32(1.0 / 11648.0)
    )
    _shape[4096, 4096, 4096, 1](
        ctx, "o", "gate", Float32(1.0 / 11648.0)
    )
    _shape[22528, 4096, 11264, 2](
        ctx, "gateup", "gate", Float32(1.0 / 11648.0)
    )
    _shape[4096, 11264, 4096, 1](
        ctx, "down", "down", Float32(1.0 / 11072.0)
    )
