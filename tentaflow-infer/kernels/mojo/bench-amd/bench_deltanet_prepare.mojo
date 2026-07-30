# =============================================================================
# Plik: bench_deltanet_prepare.mojo
# Opis: Wariant dynamiczny `deltanet_prepare` wobec wariantu rownoleglego po
#       tokenach, na ksztalcie ThinkingCap-Qwen3.6-27B (n_k=16, n_v=48,
#       d_state=128, d_conv=4). Sprawdza tez, ze oba daja BITOWO ten sam wynik.
# Przyklad: pixi run mojo run -I . bench-amd/bench_deltanet_prepare.mojo
# =============================================================================
from std.gpu.host import DeviceContext
from std.random import random_si64, seed
from std.time import perf_counter_ns

from src.deltanet_verify import (
    deltanet_prepare_dynamic_f16,
    deltanet_prepare_tokens_f16,
)

comptime WARMUP = 2
comptime ITERS = 6
comptime N_K = 16
comptime N_V = 48
comptime D_STATE = 128
comptime D_CONV = 4


def main() raises:
    seed(20260730)
    var ctx = DeviceContext()
    var t = 1024
    conv_dim = (2 * N_K + N_V) * D_STATE
    window = D_CONV - 1

    var mixed = ctx.enqueue_create_buffer[DType.float16](t * conv_dim)
    var initial = ctx.enqueue_create_buffer[DType.float16](conv_dim * window)
    var weight = ctx.enqueue_create_buffer[DType.float16](conv_dim * D_CONV)
    var alpha = ctx.enqueue_create_buffer[DType.float16](t * N_V)
    var beta = ctx.enqueue_create_buffer[DType.float16](t * N_V)
    var dt = ctx.enqueue_create_buffer[DType.float16](N_V)
    var ascale = ctx.enqueue_create_buffer[DType.float16](N_V)

    var mh = ctx.enqueue_create_host_buffer[DType.float16](t * conv_dim)
    ctx.synchronize()
    for i in range(t * conv_dim):
        mh[i] = Float16(Float64(Int(random_si64(-8, 8))) * 0.125)
    ctx.enqueue_copy(mixed, mh)
    var wh = ctx.enqueue_create_host_buffer[DType.float16](conv_dim * D_CONV)
    ctx.synchronize()
    for i in range(conv_dim * D_CONV):
        wh[i] = Float16(Float64(Int(random_si64(-4, 4))) * 0.25)
    ctx.enqueue_copy(weight, wh)
    var ih = ctx.enqueue_create_host_buffer[DType.float16](conv_dim * window)
    var ah = ctx.enqueue_create_host_buffer[DType.float16](t * N_V)
    var bh = ctx.enqueue_create_host_buffer[DType.float16](t * N_V)
    var dh = ctx.enqueue_create_host_buffer[DType.float16](N_V)
    var sh = ctx.enqueue_create_host_buffer[DType.float16](N_V)
    ctx.synchronize()
    for i in range(conv_dim * window):
        ih[i] = Float16(Float64(Int(random_si64(-4, 4))) * 0.25)
    for i in range(t * N_V):
        ah[i] = Float16(Float64(Int(random_si64(-8, 8))) * 0.125)
        bh[i] = Float16(Float64(Int(random_si64(-8, 8))) * 0.125)
    for i in range(N_V):
        dh[i] = Float16(0.125)
        sh[i] = Float16(-0.5)
    ctx.enqueue_copy(initial, ih)
    ctx.enqueue_copy(alpha, ah)
    ctx.enqueue_copy(beta, bh)
    ctx.enqueue_copy(dt, dh)
    ctx.enqueue_copy(ascale, sh)
    ctx.synchronize()

    var q = ctx.enqueue_create_buffer[DType.float16](t * N_V * D_STATE)
    var k = ctx.enqueue_create_buffer[DType.float16](t * N_V * D_STATE)
    var v = ctx.enqueue_create_buffer[DType.float16](t * N_V * D_STATE)
    var g = ctx.enqueue_create_buffer[DType.float32](t * N_V)
    var bo = ctx.enqueue_create_buffer[DType.float32](t * N_V)
    var ckpt = ctx.enqueue_create_buffer[DType.float16](t * conv_dim * window)
    var q2 = ctx.enqueue_create_buffer[DType.float16](t * N_V * D_STATE)
    var k2 = ctx.enqueue_create_buffer[DType.float16](t * N_V * D_STATE)
    var v2 = ctx.enqueue_create_buffer[DType.float16](t * N_V * D_STATE)
    var g2 = ctx.enqueue_create_buffer[DType.float32](t * N_V)
    var bo2 = ctx.enqueue_create_buffer[DType.float32](t * N_V)
    var ckpt2 = ctx.enqueue_create_buffer[DType.float16](t * conv_dim * window)

    heads = N_K + N_V
    var t0: Int = 0
    for i in range(WARMUP + ITERS):
        if i == WARMUP:
            ctx.synchronize()
            t0 = perf_counter_ns()
        ctx.enqueue_function[deltanet_prepare_dynamic_f16](
            q.unsafe_ptr(), k.unsafe_ptr(), v.unsafe_ptr(), g.unsafe_ptr(),
            bo.unsafe_ptr(), ckpt.unsafe_ptr(), initial.unsafe_ptr(),
            mixed.unsafe_ptr(), weight.unsafe_ptr(), alpha.unsafe_ptr(),
            beta.unsafe_ptr(), dt.unsafe_ptr(), ascale.unsafe_ptr(),
            t, N_K, N_V, D_STATE, D_CONV, Float32(1e-6),
            grid_dim=(heads,), block_dim=D_STATE,
        )
    ctx.synchronize()
    dyn = Float64(perf_counter_ns() - t0) / 1e9 / Float64(ITERS)
    print("dynamic (1 blok na glowe):", Int(dyn * 1e6), "us")

    var t1: Int = 0
    for i in range(WARMUP + ITERS):
        if i == WARMUP:
            ctx.synchronize()
            t1 = perf_counter_ns()
        ctx.enqueue_function[deltanet_prepare_tokens_f16[16]](
            q2.unsafe_ptr(), k2.unsafe_ptr(), v2.unsafe_ptr(), g2.unsafe_ptr(),
            bo2.unsafe_ptr(), ckpt2.unsafe_ptr(), initial.unsafe_ptr(),
            mixed.unsafe_ptr(), weight.unsafe_ptr(), alpha.unsafe_ptr(),
            beta.unsafe_ptr(), dt.unsafe_ptr(), ascale.unsafe_ptr(),
            t, N_K, N_V, D_STATE, D_CONV, Float32(1e-6),
            grid_dim=(heads, (t + 15) // 16), block_dim=D_STATE,
        )
    ctx.synchronize()
    p16 = Float64(perf_counter_ns() - t1) / 1e9 / Float64(ITERS)
    print("tokens TOK=16:", Int(p16 * 1e6), "us =", Float64(Int(dyn / p16 * 100)) / 100.0, "x")

    var t2: Int = 0
    for i in range(WARMUP + ITERS):
        if i == WARMUP:
            ctx.synchronize()
            t2 = perf_counter_ns()
        ctx.enqueue_function[deltanet_prepare_tokens_f16[32]](
            q2.unsafe_ptr(), k2.unsafe_ptr(), v2.unsafe_ptr(), g2.unsafe_ptr(),
            bo2.unsafe_ptr(), ckpt2.unsafe_ptr(), initial.unsafe_ptr(),
            mixed.unsafe_ptr(), weight.unsafe_ptr(), alpha.unsafe_ptr(),
            beta.unsafe_ptr(), dt.unsafe_ptr(), ascale.unsafe_ptr(),
            t, N_K, N_V, D_STATE, D_CONV, Float32(1e-6),
            grid_dim=(heads, (t + 31) // 32), block_dim=D_STATE,
        )
    ctx.synchronize()
    p32 = Float64(perf_counter_ns() - t2) / 1e9 / Float64(ITERS)
    print("tokens TOK=32:", Int(p32 * 1e6), "us =", Float64(Int(dyn / p32 * 100)) / 100.0, "x")

    var t3: Int = 0
    for i in range(WARMUP + ITERS):
        if i == WARMUP:
            ctx.synchronize()
            t3 = perf_counter_ns()
        ctx.enqueue_function[deltanet_prepare_tokens_f16[64]](
            q2.unsafe_ptr(), k2.unsafe_ptr(), v2.unsafe_ptr(), g2.unsafe_ptr(),
            bo2.unsafe_ptr(), ckpt2.unsafe_ptr(), initial.unsafe_ptr(),
            mixed.unsafe_ptr(), weight.unsafe_ptr(), alpha.unsafe_ptr(),
            beta.unsafe_ptr(), dt.unsafe_ptr(), ascale.unsafe_ptr(),
            t, N_K, N_V, D_STATE, D_CONV, Float32(1e-6),
            grid_dim=(heads, (t + 63) // 64), block_dim=D_STATE,
        )
    ctx.synchronize()
    p64 = Float64(perf_counter_ns() - t3) / 1e9 / Float64(ITERS)
    print("tokens TOK=64:", Int(p64 * 1e6), "us =", Float64(Int(dyn / p64 * 100)) / 100.0, "x")

    # Zgodnosc bitowa wobec wariantu dynamicznego (ostatni przebieg to TOK=64).
    var qa = ctx.enqueue_create_host_buffer[DType.float16](t * N_V * D_STATE)
    var qb = ctx.enqueue_create_host_buffer[DType.float16](t * N_V * D_STATE)
    var ga = ctx.enqueue_create_host_buffer[DType.float32](t * N_V)
    var gb = ctx.enqueue_create_host_buffer[DType.float32](t * N_V)
    ctx.enqueue_copy(qa, q)
    ctx.enqueue_copy(qb, q2)
    ctx.enqueue_copy(ga, g)
    ctx.enqueue_copy(gb, g2)
    ctx.synchronize()
    var mismatch = 0
    for i in range(t * N_V * D_STATE):
        if qa[i] != qb[i]:
            mismatch += 1
    for i in range(t * N_V):
        if ga[i] != gb[i]:
            mismatch += 1
    print("niezgodnych wartosci wobec wariantu dynamicznego:", mismatch)
