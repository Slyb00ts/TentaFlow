# ===== File: build_onnx_kernels.mojo — AOT compile the ONNX f32 kernels only =====
# Compiles just the src/onnx_ops.mojo kernels to build/<arch>/<name>.ptx and
# merges their entries into the existing manifest.json, so the large main kernel
# set does not have to be rebuilt to add the ONNX executor's ops. Run:
#   pixi run mojo build_onnx_kernels.mojo

import std.os as os
from std.gpu.host import DeviceContext
from std.pathlib import Path
from src.onnx_ops import (
    conv1d_f32,
    relu_f32,
    sigmoid_f32,
    add_f32,
    pow_f32,
    sqrt_f32,
    reduce_mean_f32,
    lstm_f32,
)


def _entry_from_ptx(ptx_path: Path) raises -> String:
    text = ptx_path.read_text()
    marker = ".visible .entry "
    i = text.find(marker)
    if i < 0:
        raise Error("no .visible .entry in " + String(ptx_path))
    j = text.find("(", i)
    if j < 0:
        raise Error("malformed entry line in " + String(ptx_path))
    return String(text[byte = i + marker.byte_length() : j])


def _finalize(out_dir: Path, name: StringSlice) raises:
    # Relocate the statically-named dump into the per-arch directory. The
    # manifest merge is done afterwards in scripts/merge_onnx_manifest.py, which
    # reads each PTX entry symbol directly (robust JSON edit, no string hacking).
    tmp = Path(String(name) + ".ptx")
    final = out_dir / (String(name) + ".ptx")
    final.write_text(tmp.read_text())
    os.remove(String(tmp))
    print("  compiled", name, "->", _entry_from_ptx(final))


def main() raises:
    var ctx = DeviceContext()
    arch = ctx.arch_name()
    out_dir = Path("build") / arch
    os.makedirs(String(out_dir), exist_ok=True)
    print("target arch:", arch)

    _ = ctx.compile_function[conv1d_f32, dump_asm = Path("conv1d_f32.ptx")]()
    _finalize(out_dir, "conv1d_f32")
    _ = ctx.compile_function[relu_f32, dump_asm = Path("relu_f32.ptx")]()
    _finalize(out_dir, "relu_f32")
    _ = ctx.compile_function[sigmoid_f32, dump_asm = Path("sigmoid_f32.ptx")]()
    _finalize(out_dir, "sigmoid_f32")
    _ = ctx.compile_function[add_f32, dump_asm = Path("add_f32.ptx")]()
    _finalize(out_dir, "add_f32")
    _ = ctx.compile_function[pow_f32, dump_asm = Path("pow_f32.ptx")]()
    _finalize(out_dir, "pow_f32")
    _ = ctx.compile_function[sqrt_f32, dump_asm = Path("sqrt_f32.ptx")]()
    _finalize(out_dir, "sqrt_f32")
    _ = ctx.compile_function[
        reduce_mean_f32, dump_asm = Path("reduce_mean_f32.ptx")
    ]()
    _finalize(out_dir, "reduce_mean_f32")
    _ = ctx.compile_function[lstm_f32, dump_asm = Path("lstm_f32.ptx")]()
    _finalize(out_dir, "lstm_f32")
    print("done — now run scripts/merge_onnx_manifest.py")
