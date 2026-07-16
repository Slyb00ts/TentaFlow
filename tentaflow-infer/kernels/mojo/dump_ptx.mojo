from std.gpu.host import DeviceContext
from std.gpu import block_dim, block_idx, thread_idx
from std.pathlib import Path

def vec_add(c: UnsafePointer[Float32, MutAnyOrigin], a: UnsafePointer[Float32, MutAnyOrigin], b: UnsafePointer[Float32, MutAnyOrigin], n: Int):
    i = block_idx.x * block_dim.x + thread_idx.x
    if i < n:
        c[i] = a[i] + b[i]

def main() raises:
    var ctx = DeviceContext()
    var f = ctx.compile_function[vec_add, dump_asm=Path("vec_add.ptx")]()
    print("compiled ok")
