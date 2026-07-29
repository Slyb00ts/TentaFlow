from std.gpu.host import DeviceContext
from std.gpu import thread_idx
from std.sys.intrinsics import llvm_intrinsic
from std.pathlib import Path
def kern(out_ptr: UnsafePointer[Float32, MutAnyOrigin], a: UnsafePointer[Int32, MutAnyOrigin]):
    av = a.load[width=2]()
    var c = SIMD[DType.float32, 8](0)
    c = llvm_intrinsic["llvm.amdgcn.wmma.f32.16x16x16.fp8.fp8.v8f32.v2i32", SIMD[DType.float32, 8]](av, av, c)
    out_ptr.store(Int(thread_idx.x) * 8, c)
def main() raises:
    var ctx = DeviceContext()
    _ = ctx.compile_function[kern, dump_asm = Path("p_fp8.ptx")]()
    print("fp8 OK")
