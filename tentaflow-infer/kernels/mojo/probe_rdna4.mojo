from std.gpu.host import DeviceContext
from std.gpu import thread_idx
from std.sys.intrinsics import llvm_intrinsic
from std.pathlib import Path


def k_fp8(out_ptr: UnsafePointer[Float32, MutAnyOrigin], a: UnsafePointer[Int32, MutAnyOrigin]):
    av = a.load[width=2]()
    var c = SIMD[DType.float32, 8](0)
    c = llvm_intrinsic[
        "llvm.amdgcn.wmma.f32.16x16x16.fp8.fp8.v8f32.v2i32", SIMD[DType.float32, 8]
    ](av, av, c)
    out_ptr.store(Int(thread_idx.x) * 8, c)


def k_bf8(out_ptr: UnsafePointer[Float32, MutAnyOrigin], a: UnsafePointer[Int32, MutAnyOrigin]):
    av = a.load[width=2]()
    var c = SIMD[DType.float32, 8](0)
    c = llvm_intrinsic[
        "llvm.amdgcn.wmma.f32.16x16x16.bf8.bf8.v8f32.v2i32", SIMD[DType.float32, 8]
    ](av, av, c)
    out_ptr.store(Int(thread_idx.x) * 8, c)


def k_iu4(out_ptr: UnsafePointer[Int32, MutAnyOrigin], a: UnsafePointer[Int32, MutAnyOrigin]):
    av = a.load[width=2]()
    var c = SIMD[DType.int32, 8](0)
    c = llvm_intrinsic[
        "llvm.amdgcn.wmma.i32.16x16x32.iu4.v8i32.v2i32", SIMD[DType.int32, 8]
    ](True, av, True, av, c, False)
    out_ptr.store(Int(thread_idx.x) * 8, c)


def k_bf16(out_ptr: UnsafePointer[Float32, MutAnyOrigin], a: UnsafePointer[BFloat16, MutAnyOrigin]):
    av = a.load[width=8]()
    var c = SIMD[DType.float32, 8](0)
    c = llvm_intrinsic[
        "llvm.amdgcn.wmma.f32.16x16x16.bf16.v8f32.v8bf16", SIMD[DType.float32, 8]
    ](av, av, c)
    out_ptr.store(Int(thread_idx.x) * 8, c)


def main() raises:
    var ctx = DeviceContext()
    print("arch:", ctx.arch_name())
    try:
        _ = ctx.compile_function[k_fp8, dump_asm = Path("p_fp8.ptx")]()
        print("  fp8 x fp8 -> f32 : JEST")
    except e:
        print("  fp8 x fp8 -> f32 : brak")
    try:
        _ = ctx.compile_function[k_bf8, dump_asm = Path("p_bf8.ptx")]()
        print("  bf8 x bf8 -> f32 : JEST")
    except e:
        print("  bf8 x bf8 -> f32 : brak")
    try:
        _ = ctx.compile_function[k_iu4, dump_asm = Path("p_iu4.ptx")]()
        print("  iu4 16x16x32     : JEST")
    except e:
        print("  iu4 16x16x32     : brak")
    try:
        _ = ctx.compile_function[k_bf16, dump_asm = Path("p_bf16.ptx")]()
        print("  bf16 16x16x16    : JEST")
    except e:
        print("  bf16 16x16x16    : brak")
