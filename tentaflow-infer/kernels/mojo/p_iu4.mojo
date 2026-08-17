from std.gpu.host import DeviceContext
from std.gpu import thread_idx
from std.sys.intrinsics import llvm_intrinsic
from std.pathlib import Path
def kern(out_ptr: UnsafePointer[Int32, MutAnyOrigin], a: UnsafePointer[Int32, MutAnyOrigin]):
    av = a.load[width=2]()
    var c = SIMD[DType.int32, 8](0)
    c = llvm_intrinsic["llvm.amdgcn.wmma.i32.16x16x32.iu4.v8i32.v2i32", SIMD[DType.int32, 8]](True, av, True, av, c, False)
    out_ptr.store(Int(thread_idx.x) * 8, c)
def main() raises:
    var ctx = DeviceContext()
    _ = ctx.compile_function[kern, dump_asm = Path("p_iu4.ptx")]()
    print("iu4 OK")
