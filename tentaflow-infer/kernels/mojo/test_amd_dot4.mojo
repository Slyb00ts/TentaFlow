# =============================================================================
# Plik: test_amd_dot4.mojo
# Opis: Sprawdza cztery składowe prymitywu int8 dot na AMD.
# Przykład: mojo run test_amd_dot4.mojo
# =============================================================================

from std.gpu import thread_idx
from std.gpu.host import DeviceContext
from src.arch_dot import dot4_i8


def dot_kernel(y: UnsafePointer[Int32, MutAnyOrigin]):
    if thread_idx.x == 0:
        y[0] = dot4_i8(Int32(0x01010101), Int32(0x7F7F7F7F), 0)


def main() raises:
    var ctx = DeviceContext()
    var output = ctx.enqueue_create_buffer[DType.int32](1)
    var host = ctx.enqueue_create_host_buffer[DType.int32](1)
    ctx.enqueue_function[dot_kernel](output.unsafe_ptr(), grid_dim=1, block_dim=32)
    ctx.enqueue_copy(host, output)
    ctx.synchronize()
    if host[0] != 508:
        raise Error("v_dot4_i32_i8 nie sumuje czterech bajtow")
    print("dot4 PASS")
