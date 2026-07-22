# =============================================================================
# Plik: test_attention_gqa.mojo
# Opis: Porównuje GQA attention z bazowym kernelem na granicach stron KV.
# Przykład: pixi run mojo run test_attention_gqa.mojo
# =============================================================================

from std.gpu.host import DeviceContext
from src.attention import attn_decode_split_f16_hd128
from src.attention import attn_decode_combine_f16_hd128
from src.attention_gqa import attn_decode_split_gqa4_f16_hd128
from src.attention_gqa_combine import attn_decode_combine_gqa2_f16_hd128

comptime NQH = 32
comptime NKVH = 8
comptime HD = 128
comptime PAGE = 32
comptime NPAGES = 4
comptime MAX_CTX = 95
comptime MAX_SPLITS = 128
comptime EPS: Float32 = 1e-5
comptime THETA: Float32 = 1000000.0
comptime SCALE: Float32 = 0.088388348


def _fill(i: Int) -> Float32:
    return Float32((i * 37 % 29) - 14) * 0.015625


def main() raises:
    var ctx = DeviceContext()
    var q = ctx.enqueue_create_buffer[DType.float16](NQH * HD)
    var k = ctx.enqueue_create_buffer[DType.float16](NKVH * HD)
    var v = ctx.enqueue_create_buffer[DType.float16](NKVH * HD)
    var qnorm = ctx.enqueue_create_buffer[DType.float16](HD)
    var knorm = ctx.enqueue_create_buffer[DType.float16](HD)
    var pt = ctx.enqueue_create_buffer[DType.int32](NPAGES)
    var slen = ctx.enqueue_create_buffer[DType.int32](1)
    var pos = ctx.enqueue_create_buffer[DType.int32](1)
    var kref = ctx.enqueue_create_buffer[DType.float16](NPAGES * NKVH * PAGE * HD)
    var vref = ctx.enqueue_create_buffer[DType.float16](NPAGES * NKVH * PAGE * HD)
    var kgqa = ctx.enqueue_create_buffer[DType.float16](NPAGES * NKVH * PAGE * HD)
    var vgqa = ctx.enqueue_create_buffer[DType.float16](NPAGES * NKVH * PAGE * HD)
    var pref = ctx.enqueue_create_buffer[DType.float32](NQH * MAX_SPLITS * (HD + 2))
    var pgqa = ctx.enqueue_create_buffer[DType.float32](NQH * MAX_SPLITS * (HD + 2))
    var oref = ctx.enqueue_create_buffer[DType.float16](NQH * HD)
    var ogqa = ctx.enqueue_create_buffer[DType.float16](NQH * HD)

    with pt.map_to_host() as hp:
        hp[0] = 2
        hp[1] = 0
        hp[2] = 3
        hp[3] = 1
    with q.map_to_host() as hq:
        for i in range(NQH * HD):
            hq[i] = Float16(_fill(i + 3))
    with k.map_to_host() as hk, v.map_to_host() as hv:
        for i in range(NKVH * HD):
            hk[i] = Float16(_fill(i + 7))
            hv[i] = Float16(_fill(i + 11))

    for ctx_case in range(5):
        ctx_len = 1 if ctx_case == 0 else (31 if ctx_case == 1 else (32 if ctx_case == 2 else (33 if ctx_case == 3 else MAX_CTX)))
        with slen.map_to_host() as hs, pos.map_to_host() as hp:
            hs[0] = Int32(ctx_len)
            hp[0] = Int32(ctx_len - 1)
        for split_case in range(6):
            splits = 1 if split_case == 0 else (3 if split_case == 1 else (8 if split_case == 2 else (32 if split_case == 3 else (64 if split_case == 4 else 128))))
            with kref.map_to_host() as kr, kgqa.map_to_host() as kg, vref.map_to_host() as vr, vgqa.map_to_host() as vg:
                for i in range(NPAGES * NKVH * PAGE * HD):
                    kv = Float16(_fill(i + 19))
                    vv = Float16(_fill(i + 23))
                    kr[i] = kv
                    kg[i] = kv
                    vr[i] = vv
                    vg[i] = vv

            ctx.enqueue_function[attn_decode_split_f16_hd128](
                pref.unsafe_ptr(), q.unsafe_ptr(), k.unsafe_ptr(), v.unsafe_ptr(),
                qnorm.unsafe_ptr(), knorm.unsafe_ptr(), kref.unsafe_ptr(), vref.unsafe_ptr(),
                pt.unsafe_ptr(), slen.unsafe_ptr(), pos.unsafe_ptr(),
                NQH, NKVH, PAGE, NPAGES, splits, 0, 0, EPS, THETA, SCALE,
                grid_dim=(1, NQH, splits), block_dim=256,
            )
            ctx.enqueue_function[attn_decode_combine_f16_hd128](
                oref.unsafe_ptr(), pref.unsafe_ptr(), NQH, splits,
                grid_dim=(1, NQH), block_dim=32,
            )
            ctx.enqueue_function[attn_decode_split_gqa4_f16_hd128](
                pgqa.unsafe_ptr(), q.unsafe_ptr(), k.unsafe_ptr(), v.unsafe_ptr(),
                qnorm.unsafe_ptr(), knorm.unsafe_ptr(), kgqa.unsafe_ptr(), vgqa.unsafe_ptr(),
                pt.unsafe_ptr(), slen.unsafe_ptr(), pos.unsafe_ptr(),
                NQH, NKVH, PAGE, NPAGES, splits, 0, 0, EPS, THETA, SCALE,
                grid_dim=(1, NKVH, splits), block_dim=256,
            )
            ctx.enqueue_function[attn_decode_combine_gqa2_f16_hd128](
                ogqa.unsafe_ptr(), pgqa.unsafe_ptr(), NQH, splits,
                grid_dim=(1, (NQH + 1) // 2), block_dim=64,
            )
            ctx.synchronize()

            var mismatches = 0
            with oref.map_to_host() as a, ogqa.map_to_host() as b:
                for i in range(NQH * HD):
                    if a[i].to_bits() != b[i].to_bits():
                        mismatches += 1
            with kref.map_to_host() as a, kgqa.map_to_host() as b, vref.map_to_host() as c, vgqa.map_to_host() as d:
                for i in range(NPAGES * NKVH * PAGE * HD):
                    if a[i].to_bits() != b[i].to_bits() or c[i].to_bits() != d[i].to_bits():
                        mismatches += 1
            print("ctx/splits/mismatches:", ctx_len, splits, mismatches)
            if mismatches != 0:
                raise Error("GQA attention różni się od bazowego kernela")
    print("PASS")
