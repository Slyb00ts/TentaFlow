# ===== File: mma_fp4.mojo — the block-scaled FP4 tensor-core op, by itself =====
# GB10 executes `mma.sync...kind::mxf4.block_scale` natively: values are e2m1
# (four bits), the scale factors ride inside the instruction, and k is 64 —
# four times the k16 an f16 tile gets after unpacking the same weights. Nothing
# in the catalogue used it, because the capability check said the part did not
# have it; ptxas and nvdisasm say otherwise.
#
# This file is ONE instruction and nothing else, on purpose. The register layout
# of a block-scaled mma is not the layout of an ordinary one — the scale
# operands are addressed by a byte within a word and a thread within a quad —
# and a layout that is wrong by one produces numbers rather than an error. So
# the instruction gets a gate of its own before any tile is built on it, and the
# gate compares against arithmetic done on the host.

from std.gpu import global_idx, thread_idx
from std.memory import UnsafePointer
from std.sys import _RegisterPackType
from std.sys._assembly import inlined_assembly


def _mma_mxf4(
    a0: UInt32,
    a1: UInt32,
    a2: UInt32,
    a3: UInt32,
    b0: UInt32,
    b1: UInt32,
    sa: UInt32,
    sb: UInt32,
    c: SIMD[DType.float32, 4],
) -> SIMD[DType.float32, 4]:
    """One m16n8k64 e2m1·e2m1 → f32 op with per-32 UE8M0 scales.

    A holds 32 four-bit values per lane in four words, B sixteen in two — the
    same register count as the m16n8k32 e4m3 op next door, because halving the
    width doubles k. `{0, 0}` selects byte 0 of the scale word and thread 0 of
    each quad as the supplier of that word.
    """
    var r = inlined_assembly[
        (
            "mma.sync.aligned.m16n8k64.row.col.kind::mxf4.block_scale"
            ".scale_vec::2X.f32.e2m1.e2m1.f32.ue8m0 {$0, $1, $2, $3},"
            " {$4, $5, $6, $7}, {$8, $9}, {$10, $11, $12, $13},"
            " {$14}, {0, 0}, {$15}, {0, 0};"
        ),
        _RegisterPackType[Float32, Float32, Float32, Float32],
        constraints="=f,=f,=f,=f,r,r,r,r,r,r,f,f,f,f,r,r",
        has_side_effect=False,
    ](a0, a1, a2, a3, b0, b1, c[0], c[1], c[2], c[3], sa, sb)
    return SIMD[DType.float32, 4](r[0], r[1], r[2], r[3])


def _mma_nvf4(
    a0: UInt32,
    a1: UInt32,
    a2: UInt32,
    a3: UInt32,
    b0: UInt32,
    b1: UInt32,
    sa: UInt32,
    sb: UInt32,
    c: SIMD[DType.float32, 4],
) -> SIMD[DType.float32, 4]:
    """The same op with per-16 UE4M3 scales instead of per-32 UE8M0 ones.

    This is the shape a `NVFP4Gguf` block already has on disk: 64 values, four
    E4M3 scales, thirty-two packed bytes. `MXFP4` has the other one, 32 values
    and one E8M0 scale. Neither needs repacking to reach the tensor core — which
    is the entire reason to prefer these two instructions over unpacking to f16.
    """
    var r = inlined_assembly[
        (
            "mma.sync.aligned.m16n8k64.row.col.kind::mxf4nvf4.block_scale"
            ".scale_vec::4X.f32.e2m1.e2m1.f32.ue4m3 {$0, $1, $2, $3},"
            " {$4, $5, $6, $7}, {$8, $9}, {$10, $11, $12, $13},"
            " {$14}, {0, 0}, {$15}, {0, 0};"
        ),
        _RegisterPackType[Float32, Float32, Float32, Float32],
        constraints="=f,=f,=f,=f,r,r,r,r,r,r,f,f,f,f,r,r",
        has_side_effect=False,
    ](a0, a1, a2, a3, b0, b1, c[0], c[1], c[2], c[3], sa, sb)
    return SIMD[DType.float32, 4](r[0], r[1], r[2], r[3])


def mma_nvf4_probe(
    d: UnsafePointer[Float32, MutAnyOrigin],
    a: UnsafePointer[UInt32, MutAnyOrigin],
    b: UnsafePointer[UInt32, MutAnyOrigin],
    sa: UnsafePointer[UInt32, MutAnyOrigin],
    sb: UnsafePointer[UInt32, MutAnyOrigin],
):
    """`mma_mxf4_probe` for the per-16 E4M3 form; same fragments, other scales."""
    lane = Int(thread_idx.x)
    if lane >= 32:
        return
    acc = _mma_nvf4(
        a[lane * 4],
        a[lane * 4 + 1],
        a[lane * 4 + 2],
        a[lane * 4 + 3],
        b[lane * 2],
        b[lane * 2 + 1],
        sa[lane],
        sb[lane],
        SIMD[DType.float32, 4](0.0, 0.0, 0.0, 0.0),
    )
    d[lane * 4] = acc[0]
    d[lane * 4 + 1] = acc[1]
    d[lane * 4 + 2] = acc[2]
    d[lane * 4 + 3] = acc[3]


def mma_mxf4_probe(
    d: UnsafePointer[Float32, MutAnyOrigin],
    a: UnsafePointer[UInt32, MutAnyOrigin],
    b: UnsafePointer[UInt32, MutAnyOrigin],
    sa: UnsafePointer[UInt32, MutAnyOrigin],
    sb: UnsafePointer[UInt32, MutAnyOrigin],
):
    """One warp, one instruction: `d[lane*4 + i]` is that lane's f32 fragment.

    The caller supplies the A and B words per lane exactly as the instruction
    wants them, so what this gates is the INSTRUCTION and the register mapping,
    not a tiling scheme. Everything built on FP4 later reuses `_mma_mxf4`.

    The scale words are per lane and not one scalar, because the selector names
    a THREAD within each quad as the supplier of a scale word: a broadcast value
    could never say which lane was read.
    """
    lane = Int(thread_idx.x)
    if lane >= 32:
        return
    acc = _mma_mxf4(
        a[lane * 4],
        a[lane * 4 + 1],
        a[lane * 4 + 2],
        a[lane * 4 + 3],
        b[lane * 2],
        b[lane * 2 + 1],
        sa[lane],
        sb[lane],
        SIMD[DType.float32, 4](0.0, 0.0, 0.0, 0.0),
    )
    d[lane * 4] = acc[0]
    d[lane * 4 + 1] = acc[1]
    d[lane * 4 + 2] = acc[2]
    d[lane * 4 + 3] = acc[3]
