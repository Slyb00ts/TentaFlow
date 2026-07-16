from std.gpu.host import DeviceContext
from std.gpu import block_idx, thread_idx, block_dim

def vec_add(c: UnsafePointer[Float32, MutAnyOrigin], a: UnsafePointer[Float32, MutAnyOrigin], b: UnsafePointer[Float32, MutAnyOrigin], n: Int):
    i = block_idx.x * block_dim.x + thread_idx.x
    if i < n:
        c[i] = a[i] + b[i]

def main() raises:
    comptime n = 1024
    var ctx = DeviceContext()
    print("device:", ctx.name())
    var a = ctx.enqueue_create_buffer[DType.float32](n)
    var b = ctx.enqueue_create_buffer[DType.float32](n)
    var c = ctx.enqueue_create_buffer[DType.float32](n)
    with a.map_to_host() as ah, b.map_to_host() as bh:
        for i in range(n):
            ah[i] = Float32(i)
            bh[i] = Float32(2 * i)
    ctx.enqueue_function[vec_add](c.unsafe_ptr(), a.unsafe_ptr(), b.unsafe_ptr(), n, grid_dim=(n + 255) // 256, block_dim=256)
    ctx.synchronize()
    with c.map_to_host() as ch:
        print("c[10] =", ch[10], "(expect 30.0)")
