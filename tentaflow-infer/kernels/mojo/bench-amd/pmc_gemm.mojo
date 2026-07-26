# =============================================================================
# Plik: pmc_gemm.mojo
# Opis: Rozbiera czas GEMM Q4_0 na skladniki. UWAGA: warianty `noepi`, `nostage`
#       i `wide` sa NARZEDZIAMI POMIAROWYMI — dwa pierwsze licza WYNIK BLEDNY
#       (celowo pomijaja epilog albo staging), zeby zmierzyc ich udzial. Nie
#       wolno ich uzyc w silniku. Zmierzone na RX 6900 XT, T=1024, N=15360,
#       K=3840: pelny 2921 us (41,3 TOPS), bez epilogu 2075 us (58,1),
#       bez stagingu 2406 us (50,1), szerszy staging 2948 us (gorszy).
# Przyklad: pixi run mojo run -I . bench-amd/pmc_gemm.mojo
# =============================================================================
from std.gpu.host import DeviceContext, DeviceBuffer
from std.gpu import block_idx, thread_idx
from std.gpu.sync import barrier
from std.gpu.memory import AddressSpace
from std.memory import stack_allocation
from std.time import perf_counter_ns
from src.arch_dot import dot4_i8
from src.gemm_dot import gemm_q4_0_dot4_impl

comptime ITERS = 30


def gemm_q4_0_noepi[BM: Int, BN: Int, TM: Int, TN: Int, KB: Int](
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    xq: UnsafePointer[Int8, MutAnyOrigin],
    xd: UnsafePointer[Float32, MutAnyOrigin],
    xsm: UnsafePointer[Float32, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """Ten sam staging i te same `v_dot4`, ale akumulacja zostaje w int32 i
    skale NIE sa nakladane. Wynik jest bledny — sluzy wylacznie do zmierzenia,
    ile szczeliny wydania zjada epilog."""
    comptime NT = (BM // TM) * (BN // TN)
    comptime ROWS_TX = BN // TN
    comptime X_PASSES = (BM + (NT // 4) - 1) // (NT // 4)
    comptime W_PASSES = (BN + (NT // 4) - 1) // (NT // 4)
    comptime XPLANE = BM * 32
    comptime WPLANE = BN * 32

    tid = Int(thread_idx.x)
    row0 = Int(block_idx.x) * BN
    t0 = Int(block_idx.y) * BM
    blocks_per_row = n_cols // 32

    xs = stack_allocation[KB * XPLANE, Int8, address_space = AddressSpace.SHARED]()
    ws = stack_allocation[KB * WPLANE, Int8, address_space = AddressSpace.SHARED]()

    lrow = tid // 4
    kc = (tid % 4) * 8
    tx = tid % ROWS_TX
    ty = tid // ROWS_TX

    var acc = InlineArray[Int32, TM * TN](fill=0)
    var base_blk = 0
    while base_blk < blocks_per_row:
        comptime for kb in range(KB):
            blk = base_blk + kb
            live = blk < blocks_per_row
            comptime for p in range(X_PASSES):
                local = lrow + p * (NT // 4)
                if local < BM:
                    var token = t0 + local
                    if token > n_tokens - 1:
                        token = n_tokens - 1
                    var bytes8 = SIMD[DType.int8, 8](0)
                    if live:
                        bytes8 = (xq + token * n_cols + blk * 32 + kc).load[width=8]()
                    comptime for q in range(2):
                        kq = kc // 4 + q
                        (xs + kb * XPLANE + kq * (BM * 4) + local * 4).store[
                            width=4, alignment=4
                        ](bytes8.slice[4, offset = q * 4]())
            comptime for p in range(W_PASSES):
                local = lrow + p * (NT // 4)
                if local < BN:
                    var row = row0 + local
                    if row > n_rows - 1:
                        row = n_rows - 1
                    var codes = SIMD[DType.int8, 8](0)
                    if live:
                        block_ptr = w + (row * blocks_per_row + blk) * 18
                        packed = (block_ptr + 2 + kc % 16).load[width=8]()
                        nib = (packed >> 4) if kc >= 16 else (packed & 0x0F)
                        codes = nib.cast[DType.int8]() - 8
                    comptime for q in range(2):
                        kq = kc // 4 + q
                        (ws + kb * WPLANE + kq * (BN * 4) + local * 4).store[
                            width=4, alignment=4
                        ](codes.slice[4, offset = q * 4]())
        barrier()
        comptime for kb in range(KB):
            xbase = xs + kb * XPLANE + ty * TM * 4
            wbase = ws + kb * WPLANE + tx * TN * 4
            comptime for kq in range(8):
                av = (xbase + kq * (BM * 4)).bitcast[Int32]().load[
                    width=TM, alignment=16
                ]()
                bv = (wbase + kq * (BN * 4)).bitcast[Int32]().load[
                    width=TN, alignment=16
                ]()
                comptime for m in range(TM):
                    comptime for r in range(TN):
                        acc[m * TN + r] = dot4_i8(av[m], bv[r], acc[m * TN + r])
        barrier()
        base_blk += KB

    comptime for m in range(TM):
        t = t0 + ty * TM + m
        if t < n_tokens:
            comptime for r in range(TN):
                row = row0 + tx * TN + r
                if row < n_rows:
                    y[t * n_rows + row] = Float16(Float32(acc[m * TN + r]))


def gemm_q4_0_nostage[BM: Int, BN: Int, TM: Int, TN: Int, KB: Int](
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    xq: UnsafePointer[Int8, MutAnyOrigin],
    xd: UnsafePointer[Float32, MutAnyOrigin],
    xsm: UnsafePointer[Float32, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """Sama petla MAC z pelnym epilogiem, BEZ zapelniania LDS (czyta smieci).
    Wynik jest bledny — mierzy koszt stagingu: odczytow globalnych, rozpakowania
    polbajtow i zapisow do LDS."""
    comptime NT = (BM // TM) * (BN // TN)
    comptime ROWS_TX = BN // TN
    comptime XPLANE = BM * 32
    comptime WPLANE = BN * 32

    tid = Int(thread_idx.x)
    row0 = Int(block_idx.x) * BN
    t0 = Int(block_idx.y) * BM
    blocks_per_row = n_cols // 32

    xs = stack_allocation[KB * XPLANE, Int8, address_space = AddressSpace.SHARED]()
    ws = stack_allocation[KB * WPLANE, Int8, address_space = AddressSpace.SHARED]()
    xds = stack_allocation[KB * BM, Float32, address_space = AddressSpace.SHARED]()
    wds = stack_allocation[KB * BN, Float32, address_space = AddressSpace.SHARED]()

    tx = tid % ROWS_TX
    ty = tid // ROWS_TX
    var acc = InlineArray[Float32, TM * TN](fill=0.0)

    var base_blk = 0
    while base_blk < blocks_per_row:
        barrier()
        comptime for kb in range(KB):
            xbase = xs + kb * XPLANE + ty * TM * 4
            wbase = ws + kb * WPLANE + tx * TN * 4
            var isum = InlineArray[Int32, TM * TN](fill=0)
            comptime for kq in range(8):
                av = (xbase + kq * (BM * 4)).bitcast[Int32]().load[
                    width=TM, alignment=16
                ]()
                bv = (wbase + kq * (BN * 4)).bitcast[Int32]().load[
                    width=TN, alignment=16
                ]()
                comptime for m in range(TM):
                    comptime for r in range(TN):
                        isum[m * TN + r] = dot4_i8(av[m], bv[r], isum[m * TN + r])
            comptime for m in range(TM):
                dx = xds[kb * BM + ty * TM + m]
                comptime for r in range(TN):
                    acc[m * TN + r] += (
                        dx * wds[kb * BN + tx * TN + r] * Float32(isum[m * TN + r])
                    )
        barrier()
        base_blk += KB

    comptime for m in range(TM):
        t = t0 + ty * TM + m
        if t < n_tokens:
            comptime for r in range(TN):
                row = row0 + tx * TN + r
                if row < n_rows:
                    y[t * n_rows + row] = acc[m * TN + r].cast[DType.float16]()


def gemm_q4_0_wide[BM: Int, BN: Int, TM: Int, TN: Int, KB: Int](
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    xq: UnsafePointer[Int8, MutAnyOrigin],
    xd: UnsafePointer[Float32, MutAnyOrigin],
    xsm: UnsafePointer[Float32, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """Ten sam wynik co `gemm_q4_0_dot4_impl`, ale SZERSZY staging: dwa watki na
    wiersz zamiast czterech. Aktywacja idzie jednym odczytem 16 B, a waga jednym
    odczytem 8 B, z ktorego watek rozpakowuje OBA polbajty (16 kodow) — dotad te
    same 16 B wagi czytaly dwa watki."""
    comptime NT = (BM // TM) * (BN // TN)
    comptime ROWS_TX = BN // TN
    comptime XPLANE = BM * 32
    comptime WPLANE = BN * 32
    comptime HALF = NT // 2

    tid = Int(thread_idx.x)
    row0 = Int(block_idx.x) * BN
    t0 = Int(block_idx.y) * BM
    blocks_per_row = n_cols // 32

    xs = stack_allocation[KB * XPLANE, Int8, address_space = AddressSpace.SHARED]()
    ws = stack_allocation[KB * WPLANE, Int8, address_space = AddressSpace.SHARED]()
    xds = stack_allocation[KB * BM, Float32, address_space = AddressSpace.SHARED]()
    wds = stack_allocation[KB * BN, Float32, address_space = AddressSpace.SHARED]()

    lrow2 = tid // 2
    kc16 = (tid % 2) * 16
    tx = tid % ROWS_TX
    ty = tid // ROWS_TX

    var acc = InlineArray[Float32, TM * TN](fill=0.0)
    var base_blk = 0
    while base_blk < blocks_per_row:
        comptime for kb in range(KB):
            blk = base_blk + kb
            live = blk < blocks_per_row
            comptime for p in range((BM + HALF - 1) // HALF):
                local = lrow2 + p * HALF
                if local < BM:
                    var token = t0 + local
                    if token > n_tokens - 1:
                        token = n_tokens - 1
                    var b16 = SIMD[DType.int8, 16](0)
                    if live:
                        b16 = (xq + token * n_cols + blk * 32 + kc16).load[width=16]()
                    comptime for q in range(4):
                        kq = kc16 // 4 + q
                        (xs + kb * XPLANE + kq * (BM * 4) + local * 4).store[
                            width=4, alignment=4
                        ](b16.slice[4, offset = q * 4]())
            comptime for p in range((BN + HALF - 1) // HALF):
                local = lrow2 + p * HALF
                if local < BN:
                    var row = row0 + local
                    if row > n_rows - 1:
                        row = n_rows - 1
                    var lo8 = SIMD[DType.int8, 8](0)
                    var hi8 = SIMD[DType.int8, 8](0)
                    var scale: Float32 = 0.0
                    if live:
                        bp = w + (row * blocks_per_row + blk) * 18
                        scale = Float32(bp.bitcast[Float16]().load[width=1]())
                        packed = (bp + 2 + (kc16 // 2)).load[width=8]()
                        lo8 = (packed & 0x0F).cast[DType.int8]() - 8
                        hi8 = (packed >> 4).cast[DType.int8]() - 8
                    comptime for q in range(2):
                        kq = kc16 // 8 + q
                        (ws + kb * WPLANE + kq * (BN * 4) + local * 4).store[
                            width=4, alignment=4
                        ](lo8.slice[4, offset = q * 4]())
                    comptime for q in range(2):
                        kq = 4 + kc16 // 8 + q
                        (ws + kb * WPLANE + kq * (BN * 4) + local * 4).store[
                            width=4, alignment=4
                        ](hi8.slice[4, offset = q * 4]())
                    if tid % 2 == 0:
                        wds[kb * BN + local] = scale
            if tid < BM:
                var token = t0 + tid
                if token > n_tokens - 1:
                    token = n_tokens - 1
                xds[kb * BM + tid] = xd[blk * n_tokens + token] if live else 0.0
        barrier()

        comptime for kb in range(KB):
            xbase = xs + kb * XPLANE + ty * TM * 4
            wbase = ws + kb * WPLANE + tx * TN * 4
            var isum = InlineArray[Int32, TM * TN](fill=0)
            comptime for kq in range(8):
                av = (xbase + kq * (BM * 4)).bitcast[Int32]().load[
                    width=TM, alignment=16
                ]()
                bv = (wbase + kq * (BN * 4)).bitcast[Int32]().load[
                    width=TN, alignment=16
                ]()
                comptime for m in range(TM):
                    comptime for r in range(TN):
                        isum[m * TN + r] = dot4_i8(av[m], bv[r], isum[m * TN + r])
            comptime for m in range(TM):
                dx = xds[kb * BM + ty * TM + m]
                comptime for r in range(TN):
                    acc[m * TN + r] += (
                        dx * wds[kb * BN + tx * TN + r] * Float32(isum[m * TN + r])
                    )
        barrier()
        base_blk += KB

    comptime for m in range(TM):
        t = t0 + ty * TM + m
        if t < n_tokens:
            comptime for r in range(TN):
                row = row0 + tx * TN + r
                if row < n_rows:
                    y[t * n_rows + row] = acc[m * TN + r].cast[DType.float16]()


def main() raises:
    var ctx = DeviceContext()
    comptime BM = 128
    comptime BN = 128
    tokens = 1024
    rows = 15360
    cols = 3840
    nb = cols // 32
    var wd = ctx.enqueue_create_buffer[DType.uint8](rows * nb * 18)
    var xdb = ctx.enqueue_create_buffer[DType.int8](tokens * cols)
    var ddb = ctx.enqueue_create_buffer[DType.float32](nb * tokens)
    var sdb = ctx.enqueue_create_buffer[DType.float32](nb * tokens)
    var yd = ctx.enqueue_create_buffer[DType.float16](tokens * rows)
    ctx.synchronize()
    grid = ((rows + BN - 1) // BN, (tokens + BM - 1) // BM)
    blk = (BM // 8) * (BN // 4)
    ops = 2.0 * Float64(tokens) * Float64(rows) * Float64(cols)

    for _ in range(3):
        ctx.enqueue_function[gemm_q4_0_dot4_impl[BM, BN, 8, 4, 2, DType.float16]](
            yd.unsafe_ptr(), wd.unsafe_ptr(), xdb.unsafe_ptr(), ddb.unsafe_ptr(),
            sdb.unsafe_ptr(), cols, rows, tokens, grid_dim=grid, block_dim=blk)
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemm_q4_0_dot4_impl[BM, BN, 8, 4, 2, DType.float16]](
            yd.unsafe_ptr(), wd.unsafe_ptr(), xdb.unsafe_ptr(), ddb.unsafe_ptr(),
            sdb.unsafe_ptr(), cols, rows, tokens, grid_dim=grid, block_dim=blk)
    ctx.synchronize()
    dt = Float64(perf_counter_ns() - t0) / 1e9 / ITERS
    print("pelny   ", Int(dt * 1e6), "us", Int(ops / dt / 1e11), "/10 TOPS")

    for _ in range(3):
        ctx.enqueue_function[gemm_q4_0_noepi[BM, BN, 8, 4, 2]](
            yd.unsafe_ptr(), wd.unsafe_ptr(), xdb.unsafe_ptr(), ddb.unsafe_ptr(),
            sdb.unsafe_ptr(), cols, rows, tokens, grid_dim=grid, block_dim=blk)
    ctx.synchronize()
    t1 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemm_q4_0_noepi[BM, BN, 8, 4, 2]](
            yd.unsafe_ptr(), wd.unsafe_ptr(), xdb.unsafe_ptr(), ddb.unsafe_ptr(),
            sdb.unsafe_ptr(), cols, rows, tokens, grid_dim=grid, block_dim=blk)
    ctx.synchronize()
    dt2 = Float64(perf_counter_ns() - t1) / 1e9 / ITERS
    print("bez epi ", Int(dt2 * 1e6), "us", Int(ops / dt2 / 1e11), "/10 TOPS")

    for _ in range(3):
        ctx.enqueue_function[gemm_q4_0_nostage[BM, BN, 8, 4, 2]](
            yd.unsafe_ptr(), wd.unsafe_ptr(), xdb.unsafe_ptr(), ddb.unsafe_ptr(),
            sdb.unsafe_ptr(), cols, rows, tokens, grid_dim=grid, block_dim=blk)
    ctx.synchronize()
    t2 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemm_q4_0_nostage[BM, BN, 8, 4, 2]](
            yd.unsafe_ptr(), wd.unsafe_ptr(), xdb.unsafe_ptr(), ddb.unsafe_ptr(),
            sdb.unsafe_ptr(), cols, rows, tokens, grid_dim=grid, block_dim=blk)
    ctx.synchronize()
    dt3 = Float64(perf_counter_ns() - t2) / 1e9 / ITERS
    print("bez stag", Int(dt3 * 1e6), "us", Int(ops / dt3 / 1e11), "/10 TOPS")

    for _ in range(3):
        ctx.enqueue_function[gemm_q4_0_wide[BM, BN, 8, 4, 2]](
            yd.unsafe_ptr(), wd.unsafe_ptr(), xdb.unsafe_ptr(), ddb.unsafe_ptr(),
            sdb.unsafe_ptr(), cols, rows, tokens, grid_dim=grid, block_dim=blk)
    ctx.synchronize()
    t3 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemm_q4_0_wide[BM, BN, 8, 4, 2]](
            yd.unsafe_ptr(), wd.unsafe_ptr(), xdb.unsafe_ptr(), ddb.unsafe_ptr(),
            sdb.unsafe_ptr(), cols, rows, tokens, grid_dim=grid, block_dim=blk)
    ctx.synchronize()
    dt4 = Float64(perf_counter_ns() - t3) / 1e9 / ITERS
    print("szeroki ", Int(dt4 * 1e6), "us", Int(ops / dt4 / 1e11), "/10 TOPS")
    for _ in range(3):
        ctx.enqueue_function[gemm_q4_0_dot4_impl[128, 128, 8, 4, 1, DType.float16]](
            yd.unsafe_ptr(), wd.unsafe_ptr(), xdb.unsafe_ptr(), ddb.unsafe_ptr(),
            sdb.unsafe_ptr(), cols, rows, tokens,
            grid_dim=((rows + 128 - 1) // 128, (tokens + 128 - 1) // 128),
            block_dim=(128 // 8) * (128 // 4))
    ctx.synchronize()
    tk0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemm_q4_0_dot4_impl[128, 128, 8, 4, 1, DType.float16]](
            yd.unsafe_ptr(), wd.unsafe_ptr(), xdb.unsafe_ptr(), ddb.unsafe_ptr(),
            sdb.unsafe_ptr(), cols, rows, tokens,
            grid_dim=((rows + 128 - 1) // 128, (tokens + 128 - 1) // 128),
            block_dim=(128 // 8) * (128 // 4))
    ctx.synchronize()
    dk0 = Float64(perf_counter_ns() - tk0) / 1e9 / ITERS
    print("KB1 128x128", Int(dk0 * 1e6), "us", Int(ops / dk0 / 1e11), "/10 TOPS")

    for _ in range(3):
        ctx.enqueue_function[gemm_q4_0_dot4_impl[128, 64, 8, 4, 1, DType.float16]](
            yd.unsafe_ptr(), wd.unsafe_ptr(), xdb.unsafe_ptr(), ddb.unsafe_ptr(),
            sdb.unsafe_ptr(), cols, rows, tokens,
            grid_dim=((rows + 64 - 1) // 64, (tokens + 128 - 1) // 128),
            block_dim=(128 // 8) * (64 // 4))
    ctx.synchronize()
    tk1 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemm_q4_0_dot4_impl[128, 64, 8, 4, 1, DType.float16]](
            yd.unsafe_ptr(), wd.unsafe_ptr(), xdb.unsafe_ptr(), ddb.unsafe_ptr(),
            sdb.unsafe_ptr(), cols, rows, tokens,
            grid_dim=((rows + 64 - 1) // 64, (tokens + 128 - 1) // 128),
            block_dim=(128 // 8) * (64 // 4))
    ctx.synchronize()
    dk1 = Float64(perf_counter_ns() - tk1) / 1e9 / ITERS
    print("KB1 128x64 ", Int(dk1 * 1e6), "us", Int(ops / dk1 / 1e11), "/10 TOPS")

    for _ in range(3):
        ctx.enqueue_function[gemm_q4_0_dot4_impl[64, 128, 4, 4, 1, DType.float16]](
            yd.unsafe_ptr(), wd.unsafe_ptr(), xdb.unsafe_ptr(), ddb.unsafe_ptr(),
            sdb.unsafe_ptr(), cols, rows, tokens,
            grid_dim=((rows + 128 - 1) // 128, (tokens + 64 - 1) // 64),
            block_dim=(64 // 4) * (128 // 4))
    ctx.synchronize()
    tk2 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemm_q4_0_dot4_impl[64, 128, 4, 4, 1, DType.float16]](
            yd.unsafe_ptr(), wd.unsafe_ptr(), xdb.unsafe_ptr(), ddb.unsafe_ptr(),
            sdb.unsafe_ptr(), cols, rows, tokens,
            grid_dim=((rows + 128 - 1) // 128, (tokens + 64 - 1) // 64),
            block_dim=(64 // 4) * (128 // 4))
    ctx.synchronize()
    dk2 = Float64(perf_counter_ns() - tk2) / 1e9 / ITERS
    print("KB1 64x128 ", Int(dk2 * 1e6), "us", Int(ops / dk2 / 1e11), "/10 TOPS")

    for _ in range(3):
        ctx.enqueue_function[gemm_q4_0_dot4_impl[64, 64, 4, 4, 1, DType.float16]](
            yd.unsafe_ptr(), wd.unsafe_ptr(), xdb.unsafe_ptr(), ddb.unsafe_ptr(),
            sdb.unsafe_ptr(), cols, rows, tokens,
            grid_dim=((rows + 64 - 1) // 64, (tokens + 64 - 1) // 64),
            block_dim=(64 // 4) * (64 // 4))
    ctx.synchronize()
    tk3 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemm_q4_0_dot4_impl[64, 64, 4, 4, 1, DType.float16]](
            yd.unsafe_ptr(), wd.unsafe_ptr(), xdb.unsafe_ptr(), ddb.unsafe_ptr(),
            sdb.unsafe_ptr(), cols, rows, tokens,
            grid_dim=((rows + 64 - 1) // 64, (tokens + 64 - 1) // 64),
            block_dim=(64 // 4) * (64 // 4))
    ctx.synchronize()
    dk3 = Float64(perf_counter_ns() - tk3) / 1e9 / ITERS
    print("KB1 64x64  ", Int(dk3 * 1e6), "us", Int(ops / dk3 / 1e11), "/10 TOPS")


