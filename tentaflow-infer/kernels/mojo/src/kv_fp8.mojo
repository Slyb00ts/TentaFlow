# ===== File: kv_fp8.mojo — fast e4m3 widening for FP8 KV cache reads =====
# Mojo's generic float8 -> float cast lowers to 64-bit bit-math emulation
# (hundreds of extra instructions in the attention inner loops). sm_89+ has a
# hardware pair conversion — cvt.rn.f16x2.e4m3x2 — and e4m3 -> f16 widening
# is exact, so these helpers keep the fp8 read path bit-identical to "f16
# kernel on a dequantized cache" while restoring the bandwidth win.

from std.memory import bitcast
from std.sys import inlined_assembly

comptime WARP_SIZE = 32


def _e4m3x2_to_f16x2(lo: UInt8, hi: UInt8) -> SIMD[DType.float16, 2]:
    pair = inlined_assembly[
        "cvt.rn.f16x2.e4m3x2 $0, $1;",
        UInt32,
        constraints="=r,h",
        has_side_effect=False,
    ](UInt16(UInt32(lo) | (UInt32(hi) << 8)))
    return bitcast[DType.float16, 2](pair)


def kv_frag_f32[kv_dtype: DType, epl: Int](
    cache: UnsafePointer[Scalar[kv_dtype], MutAnyOrigin],
    base: Int,
    lane: Int,
) -> SIMD[DType.float32, epl]:
    """One lane's strided fragment of a cached row (element e lives at
    base + e*32 + lane), widened to f32. epl must be even for the fp8 path
    (head_dim is a multiple of 64 across supported specializations)."""
    var out = SIMD[DType.float32, epl](0.0)

    comptime if kv_dtype == DType.float8_e4m3fn:
        comptime for j in range(epl // 2):
            lo = cache[base + (2 * j) * WARP_SIZE + lane].to_bits[DType.uint8]()
            hi = cache[base + (2 * j + 1) * WARP_SIZE + lane].to_bits[
                DType.uint8
            ]()
            f2 = _e4m3x2_to_f16x2(lo, hi)
            out[2 * j] = Float32(f2[0])
            out[2 * j + 1] = Float32(f2[1])
    else:
        comptime for e in range(epl):
            out[e] = Float32(cache[base + e * WARP_SIZE + lane])
    return out


def kv_row8_f16[kv_dtype: DType](
    ptr: UnsafePointer[Scalar[kv_dtype], MutAnyOrigin]
) -> SIMD[DType.float16, 8]:
    """Eight consecutive cache elements widened to f16 (for shared-memory
    tile staging). ptr must be 8-element aligned."""

    comptime if kv_dtype == DType.float8_e4m3fn:
        raw = ptr.bitcast[UInt16]().load[width=4, alignment=8]()
        var pairs = SIMD[DType.uint32, 4](0)

        comptime for j in range(4):
            pairs[j] = inlined_assembly[
                "cvt.rn.f16x2.e4m3x2 $0, $1;",
                UInt32,
                constraints="=r,h",
                has_side_effect=False,
            ](raw[j])
        return bitcast[DType.float16, 8](pairs)
    else:
        return ptr.load[width=8, alignment=16]().cast[DType.float16]()
