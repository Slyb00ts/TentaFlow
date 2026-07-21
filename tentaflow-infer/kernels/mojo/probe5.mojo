from std.gpu.host import DeviceContext
from std.gpu import block_idx, thread_idx

def vec_probe(y: UnsafePointer[Float32, MutAnyOrigin], x: UnsafePointer[Float16, MutAnyOrigin], w: UnsafePointer[UInt8, MutAnyOrigin]):
    xv = x.load[width=8](Int(thread_idx.x) * 8)
    qv = w.load[width=16](0)
    q8 = qv.slice[8]().cast[DType.int8]().cast[DType.float32]()
    acc = (xv.cast[DType.float32]() * q8).reduce_add()
    y[Int(thread_idx.x)] = acc

def main() raises:
    var ctx = DeviceContext()
    var y = ctx.enqueue_create_buffer[DType.float32](32)
    var x = ctx.enqueue_create_buffer[DType.float16](256)
    var w = ctx.enqueue_create_buffer[DType.uint8](256)
    ctx.enqueue_function[vec_probe](y.unsafe_ptr(), x.unsafe_ptr(), w.unsafe_ptr(), grid_dim=1, block_dim=32)
    ctx.synchronize()
    print("simd load ok")
