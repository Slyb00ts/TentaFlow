# =============================================================================
# Plik: bench_nvfp4_ct_prefill_kv.mojo
# Opis: Porównuje prefill K/V Row i S0 na realnych wagach Bielika dla 40 warstw.
# Przykład: pixi run mojo bench_nvfp4_ct_prefill_kv.mojo
# =============================================================================

from std.gpu.host import DeviceBuffer, DeviceContext
from std.python import Python
from std.time import perf_counter_ns
from src.gemm import gemm_nvfp4_impl
from src.nvfp4_ct_layout import repack_nvfp4_ct_s0_n64k128_into
from src.nvfp4_ct_prefill import gemm_nvfp4_ct_s0_impl

comptime ROWS = 6144
comptime COLS = 4096
comptime WINDOW_ROWS = 1024
comptime PACKED_BYTES = ROWS * COLS // 2
comptime SCALE_BYTES = ROWS * COLS // 16
comptime RESIDENT_BYTES = PACKED_BYTES + SCALE_BYTES
comptime LAYERS = 40
comptime WARMUP = 3
comptime ITERS = 5
comptime ROUNDS = 7
comptime INV_GLOBAL_SCALE = 1.0 / 11648.0
comptime DATA_ROOT = (
    "/home/critix/.cache/tentaflow-profiles/"
    "nvfp4-marlin-v2-study/v2/data/gate/"
)


def _load(mut buffer: DeviceBuffer[DType.uint8], path: String) raises:
    builtins = Python.import_module("builtins")
    ctypes = Python.import_module("ctypes")
    data = builtins.open(path, "rb").read()
    if Int(data.__len__()) < len(buffer):
        raise Error("za mało danych w pliku " + path)
    with buffer.map_to_host() as target:
        _ = ctypes.memmove(Int(target.unsafe_ptr()), data, len(buffer))


def _row[tokens: Int, BM: Int, NW: Int, source_row_offset: Int](
    ctx: DeviceContext,
    mut output: DeviceBuffer[DType.float16],
    mut packed: DeviceBuffer[DType.uint8],
    mut scales: DeviceBuffer[DType.uint8],
    mut x: DeviceBuffer[DType.float16],
) raises:
    ctx.enqueue_function[gemm_nvfp4_impl[BM, NW]](
        output.unsafe_ptr(),
        packed.unsafe_ptr() + source_row_offset * (COLS // 2),
        scales.unsafe_ptr() + source_row_offset * (COLS // 16),
        x.unsafe_ptr(),
        COLS,
        WINDOW_ROWS,
        tokens,
        Float32(INV_GLOBAL_SCALE),
        grid_dim=(WINDOW_ROWS // 64, (tokens + BM - 1) // BM),
        block_dim=NW * 32,
    )


def _s0[tokens: Int, BM: Int, NW: Int, source_row_offset: Int](
    ctx: DeviceContext,
    mut output: DeviceBuffer[DType.float16],
    mut resident: DeviceBuffer[DType.uint8],
    mut x: DeviceBuffer[DType.float16],
) raises:
    ctx.enqueue_function[gemm_nvfp4_ct_s0_impl[BM, NW]](
        output.unsafe_ptr(),
        resident.unsafe_ptr(),
        x.unsafe_ptr(),
        COLS,
        WINDOW_ROWS,
        tokens,
        source_row_offset,
        Float32(INV_GLOBAL_SCALE),
        grid_dim=(WINDOW_ROWS // 64, (tokens + BM - 1) // BM),
        block_dim=NW * 32,
    )


def _median(mut values: InlineArray[Float64, ROUNDS]) -> Float64:
    for left in range(ROUNDS):
        for right in range(left + 1, ROUNDS):
            if values[right] < values[left]:
                values[left], values[right] = values[right], values[left]
    return values[ROUNDS // 2]


def _case[tokens: Int, BM: Int, NW: Int, source_row_offset: Int](
    ctx: DeviceContext,
    name: String,
    mut packed: DeviceBuffer[DType.uint8],
    mut scales: DeviceBuffer[DType.uint8],
    mut resident: DeviceBuffer[DType.uint8],
) raises:
    var x = ctx.enqueue_create_buffer[DType.float16](tokens * COLS)
    var row_output = ctx.enqueue_create_buffer[DType.float16](
        tokens * WINDOW_ROWS
    )
    var s0_output = ctx.enqueue_create_buffer[DType.float16](
        tokens * WINDOW_ROWS
    )
    with x.map_to_host() as values:
        for i in range(len(values)):
            values[i] = Float16(
                Float32((i * 17 + 13) % 127 - 63) * 0.00390625
            )

    _row[tokens, BM, NW, source_row_offset](
        ctx, row_output, packed, scales, x
    )
    _s0[tokens, BM, NW, source_row_offset](ctx, s0_output, resident, x)
    ctx.synchronize()
    var mismatches = 0
    with row_output.map_to_host() as expected, s0_output.map_to_host() as actual:
        for i in range(len(expected)):
            if expected[i] != actual[i]:
                mismatches += 1
    if mismatches != 0:
        raise Error("wynik S0 nie jest bitowo zgodny z Row")

    for _ in range(WARMUP):
        for _ in range(LAYERS):
            _row[tokens, BM, NW, source_row_offset](
                ctx, row_output, packed, scales, x
            )
        for _ in range(LAYERS):
            _s0[tokens, BM, NW, source_row_offset](
                ctx, s0_output, resident, x
            )
    ctx.synchronize()

    var row_rounds = InlineArray[Float64, ROUNDS](uninitialized=True)
    var s0_rounds = InlineArray[Float64, ROUNDS](uninitialized=True)
    for round_index in range(ROUNDS):
        var started = perf_counter_ns()
        for _ in range(ITERS):
            for _ in range(LAYERS):
                _row[tokens, BM, NW, source_row_offset](
                    ctx, row_output, packed, scales, x
                )
        ctx.synchronize()
        row_rounds[round_index] = (
            Float64(perf_counter_ns() - started) / Float64(ITERS)
        )

        started = perf_counter_ns()
        for _ in range(ITERS):
            for _ in range(LAYERS):
                _s0[tokens, BM, NW, source_row_offset](
                    ctx, s0_output, resident, x
                )
        ctx.synchronize()
        s0_rounds[round_index] = (
            Float64(perf_counter_ns() - started) / Float64(ITERS)
        )

    row_ns = _median(row_rounds)
    s0_ns = _median(s0_rounds)
    print(
        name,
        "T", tokens,
        "BM", BM,
        "row_kernel_us", row_ns / Float64(LAYERS * 1000),
        "s0_kernel_us", s0_ns / Float64(LAYERS * 1000),
        "row_40_layers_ms", row_ns / 1000000.0,
        "s0_40_layers_ms", s0_ns / 1000000.0,
        "s0_over_row", s0_ns / row_ns,
        "mismatches", mismatches,
    )


def main() raises:
    var ctx = DeviceContext()
    var packed = ctx.enqueue_create_buffer[DType.uint8](PACKED_BYTES)
    var scales = ctx.enqueue_create_buffer[DType.uint8](SCALE_BYTES)
    var resident = ctx.enqueue_create_buffer[DType.uint8](RESIDENT_BYTES)
    _load(packed, DATA_ROOT + "weight_packed.bin")
    _load(scales, DATA_ROOT + "weight_scale.bin")
    ctx.enqueue_function[repack_nvfp4_ct_s0_n64k128_into](
        resident.unsafe_ptr(),
        packed.unsafe_ptr(),
        scales.unsafe_ptr(),
        COLS,
        ROWS,
        0,
        grid_dim=(ROWS // 64 * (COLS // 128),),
        block_dim=128,
    )
    ctx.synchronize()

    _case[64, 64, 4, 4096](ctx, "K", packed, scales, resident)
    _case[128, 64, 4, 4096](ctx, "K", packed, scales, resident)
    _case[128, 128, 8, 4096](ctx, "K", packed, scales, resident)
    _case[64, 64, 4, 5120](ctx, "V", packed, scales, resident)
    _case[128, 64, 4, 5120](ctx, "V", packed, scales, resident)
    _case[128, 128, 8, 5120](ctx, "V", packed, scales, resident)
