#!/usr/bin/env python3
# =============================================================================
# Plik: eks_a9_gen.py
# Opis: Generator modeli CoreML do eksperymentu EKS-A9 (ANE jako trzecia
#       jednostka prefillu). Buduje wycinek FFN 7B (gate/up/silu/down) o stałym
#       kształcie T x 4096 dla T = 256/512/1024 w trzech wariantach wag (fp16,
#       int4 per-channel, LUT 4-bit), model trywialny do pomiaru narzutu
#       dyspozycji oraz warianty z wejściem/wyjściem obrazowym (IOSurface).
#       Zapisuje też wagi zdekwantyzowane dokładnie tak, jak je przechowuje
#       coremltools, do referencji fp32 liczonej w harnessie Swift.
#       Tryb --a10 (EKS-A10 faza 0): warianty kodowania 4-bit blokowego
#       (grupa 64 wzdłuż K, jak MLX affine) przez constexpr_blockwise_shift_scale,
#       kontrola constexpr_affine_dequantize (iOS16, per-channel) oraz jeden
#       mlpackage multifunction z funkcjami T256/T512/T1024 na wspólnych wagach.
# Przykład: venv/bin/python eks_a9_gen.py /sciezka/na/modele
#           venv/bin/python eks_a9_gen.py /sciezka/na/modele_a10 --a10
# =============================================================================

import os
import shutil
import subprocess
import sys
import time

import numpy as np
import coremltools as ct
from coremltools.converters.mil import Builder as mb
from coremltools.converters.mil.mil import types
from coremltools.converters.mil.mil.types.type_mapping import np_uint4_dtype
from coremltools.models.utils import MultiFunctionDescriptor, save_multifunction
from coremltools.optimize.coreml import (
    OpLinearQuantizerConfig,
    OpPalettizerConfig,
    OptimizationConfig,
    decompress_weights,
    linear_quantize_weights,
    palettize_weights,
)

D_MODEL = 4096
N_SLICE = 3072          # wycinek wymiaru inter (11264) liczony przez ANE
SHAPES_T = [256, 512, 1024]
SEED = 20260911
GROUP = 64              # grupa kwantyzacji wzdłuż K (MLX affine 4-bit, group_size=64)


def log(msg):
    print(f"[gen] {msg}", flush=True)


def make_weights(rng):
    """Wagi z rozkładu podobnego do LLM (normalny, std 0,02), zaokrąglone do f16."""
    w_gate = rng.normal(0.0, 0.02, (N_SLICE, D_MODEL)).astype(np.float16)
    w_up = rng.normal(0.0, 0.02, (N_SLICE, D_MODEL)).astype(np.float16)
    w_down = rng.normal(0.0, 0.02, (D_MODEL, N_SLICE)).astype(np.float16)
    return w_gate, w_up, w_down


def quantize_affine_group(w, group=GROUP):
    """Kwantyzacja affine 4-bit jak w MLX (mx.quantize, bits=4, group_size=64):
    w ≈ q·scale + bias, q ∈ [0,15], scale = (max−min)/15 i bias = min w grupie
    `group` kolejnych elementów wzdłuż K; scale i bias zapisane w f16 (MLX trzyma je
    w dtype wag). Zwraca q uint8 [rows,K], scale f16 [rows,K/group], bias f16 [rows,K/group]."""
    rows, k = w.shape
    w32 = w.astype(np.float32).reshape(rows, k // group, group)
    wmin = w32.min(axis=2, keepdims=True)
    wmax = w32.max(axis=2, keepdims=True)
    scale = (wmax - wmin) / 15.0
    scale = np.where(scale == 0, 1e-7, scale)
    q = np.clip(np.round((w32 - wmin) / scale), 0, 15).astype(np.uint8)
    return (q.reshape(rows, k), scale.astype(np.float16).reshape(rows, k // group),
            wmin.astype(np.float16).reshape(rows, k // group))


def dequant_mlx(q, scale, bias, group=GROUP):
    """Dekwantyzacja wg wzoru MLX: q·scale + bias (f32 z parametrów f16)."""
    rows, k = q.shape
    s = np.repeat(scale.astype(np.float32), group, axis=1)
    b = np.repeat(bias.astype(np.float32), group, axis=1)
    return q.astype(np.float32) * s + b


def blockwise_const(q, scale, bias, name, offset_dtype):
    """Stała 4-bitowa przez constexpr_blockwise_shift_scale (opset iOS18):
    output = scale·(data − offset). Dla offset f16: offset = −bias/scale (bit-w-bit
    to samo co q·scale+bias tylko z dokładnością f16 ilorazu). Dla offset uint4:
    zero-point całkowity z = round(−bias/scale) ∈ [0,15], q przeliczone od nowa —
    NIE jest bit-exact wobec MLX."""
    scale32 = scale.astype(np.float32)
    if offset_dtype == "fp16":
        offset = (-bias.astype(np.float32) / scale32).astype(np.float16)
        data = q
    else:
        z = np.clip(np.round(-bias.astype(np.float32) / scale32), 0, 15)
        # w = scale·(q − z)  ⇒  q_new = round(w/scale) + z, gdzie w = q·scale + bias
        w = dequant_mlx(q, scale, bias)
        s_full = np.repeat(scale32, GROUP, axis=1)
        z_full = np.repeat(z, GROUP, axis=1)
        data = np.clip(np.round(w / s_full) + z_full, 0, 15).astype(np.uint8)
        offset = z.astype(np.uint8).astype(np_uint4_dtype)
    return mb.constexpr_blockwise_shift_scale(data=data.astype(np_uint4_dtype), scale=scale,
                                              offset=offset, name=name)


def affine16_const(w, name):
    """Kontrola: constexpr_affine_dequantize (opset iOS16) — w coremltools 9.0 ten op
    przyjmuje wyłącznie int8/uint8 (4-bit dopiero w iOS18), więc kontrola jest int8
    symetryczna per-channel (skala na wiersz, zero_point 0)."""
    w32 = w.astype(np.float32)
    amax = np.abs(w32).max(axis=1, keepdims=True)
    scale = np.where(amax == 0, 1e-7, amax / 127.0)
    q = np.clip(np.round(w32 / scale), -128, 127).astype(np.int8)
    return mb.constexpr_affine_dequantize(quantized_data=q,
                                          zero_point=np.zeros((w.shape[0],), dtype=np.int8),
                                          scale=scale.astype(np.float16).reshape(-1), axis=0, name=name)


def build_ffn_program(t_rows, weights, rank4, encoding="fp16"):
    """Program MIL: y = silu(x Wg^T) * (x Wu^T), potem y Wd^T. rank4 = wejście [1,1,T,4096]
    (pod ImageType), w przeciwnym razie [T,4096]. encoding: fp16 (stałe gęste, do
    późniejszej kompresji przez coremltools), blockwise (grupa 64, offset f16),
    blockwise_zp (grupa 64, offset uint4), affine16 (int8 per-channel, opset iOS16)."""
    w_gate, w_up, w_down = weights
    in_shape = (1, 1, t_rows, D_MODEL) if rank4 else (t_rows, D_MODEL)

    def wconst(w, name):
        if encoding == "fp16":
            return mb.const(val=w, name=name)
        if encoding == "affine16":
            return affine16_const(w, name)
        return blockwise_const(*quantize_affine_group(w), name,
                               "fp16" if encoding == "blockwise" else "uint4")

    @mb.program(input_specs=[mb.TensorSpec(shape=in_shape, dtype=types.fp16)],
                opset_version=ct.target.macOS15)
    def prog(x):
        h = mb.reshape(x=x, shape=(t_rows, D_MODEL)) if rank4 else x
        gate = mb.linear(x=h, weight=wconst(w_gate, "w_gate"), name="gate")
        up = mb.linear(x=h, weight=wconst(w_up, "w_up"), name="up")
        act = mb.mul(x=mb.silu(x=gate), y=up, name="act")
        out = mb.linear(x=act, weight=wconst(w_down, "w_down"), name="down")
        if rank4:
            out = mb.reshape(x=out, shape=(1, 1, t_rows, D_MODEL))
        return mb.identity(x=out, name="y")

    return prog


def build_trivial_program(rows, dim):
    """Model trywialny [rows, dim] x [dim, dim]: mierzy czysty narzut jednego predict.
    Najmniejsze kształty CoreML kieruje na CPU, więc generowanych jest kilka rozmiarów,
    a harness sprawdza przez MLComputePlan, który z nich pierwszy ląduje na ANE."""
    rng = np.random.default_rng(SEED + 1)
    w = rng.normal(0.0, 0.1, (dim, dim)).astype(np.float16)

    @mb.program(input_specs=[mb.TensorSpec(shape=(rows, dim), dtype=types.fp16)],
                opset_version=ct.target.macOS15)
    def prog(x):
        return mb.identity(x=mb.linear(x=x, weight=w), name="y")

    return prog


TRIVIAL_SHAPES = [(1, 64), (1, 256), (8, 256), (1, 1024), (8, 1024), (64, 1024),
                  (128, 1024), (256, 1024), (512, 1024), (64, 2048), (64, 4096)]


def convert(prog):
    return ct.convert(prog, source="milinternal", convert_to="mlprogram",
                      compute_precision=ct.precision.FLOAT16,
                      minimum_deployment_target=ct.target.macOS15,
                      skip_model_load=True)


def quantize_int4(model):
    cfg = OptimizationConfig(global_config=OpLinearQuantizerConfig(
        mode="linear_symmetric", dtype="int4", granularity="per_channel"))
    return linear_quantize_weights(model, cfg)


def palettize_lut4(model):
    cfg = OptimizationConfig(global_config=OpPalettizerConfig(
        mode="kmeans", nbits=4, granularity="per_tensor",
        num_kmeans_workers=1))  # >1 wywala się w coremltools 9.0 ("Pool not running")
    return palettize_weights(model, cfg)


def set_image_io(model, t_rows):
    """Przełącza wejście i wyjście na obraz OneComponent16Half [T wierszy, 4096 kolumn].
    Graf MIL zostaje ten sam ([1,1,T,4096] fp16) — zmienia się tylko opis interfejsu,
    dokładnie tak, jak robi to ct.convert z ImageType."""
    spec = model.get_spec()
    for feat in list(spec.description.input) + list(spec.description.output):
        feat.type.imageType.width = D_MODEL
        feat.type.imageType.height = t_rows
        feat.type.imageType.colorSpace = ct.proto.FeatureTypes_pb2.ImageFeatureType.GRAYSCALE_FLOAT16
    return ct.models.MLModel(spec, weights_dir=model.weights_dir, skip_model_load=True)


def extract_dense_weights(model):
    """Wyciąga wagi w postaci gęstej (po dekompresji) po nazwach stałych."""
    dense = decompress_weights(model)
    found = {}
    for op in dense._mil_program.functions["main"].operations:
        if op.op_type == "const" and op.name in ("w_gate", "w_up", "w_down"):
            found[op.name] = np.asarray(op.outputs[0].val, dtype=np.float32)
    if len(found) != 3:
        # Nazwy mogły zostać zmienione przez passy — dopasuj po kształcie i kolejności.
        by_shape = [(op.name, np.asarray(op.outputs[0].val, dtype=np.float32))
                    for op in dense._mil_program.functions["main"].operations
                    if op.op_type == "const" and op.outputs[0].val is not None
                    and np.asarray(op.outputs[0].val).size == N_SLICE * D_MODEL]
        names = ["w_gate", "w_up", "w_down"]
        found = {n: v for n, (_, v) in zip(names, by_shape)}
    return found


def save_and_compile(model, out_dir, name):
    pkg = os.path.join(out_dir, f"{name}.mlpackage")
    if os.path.exists(pkg):
        shutil.rmtree(pkg)
    model.save(pkg)
    t0 = time.time()
    subprocess.run(["xcrun", "coremlcompiler", "compile", pkg, out_dir], check=True,
                   stdout=subprocess.DEVNULL)
    size = 0
    for root, _, files in os.walk(os.path.join(out_dir, f"{name}.mlmodelc")):
        for f in files:
            size += os.path.getsize(os.path.join(root, f))
    log(f"{name}: mlmodelc {size / 2**20:.1f} MiB, kompilacja {time.time() - t0:.1f} s")
    return size


def main():
    out_dir = sys.argv[1] if len(sys.argv) > 1 else "models"
    os.makedirs(out_dir, exist_ok=True)
    rng = np.random.default_rng(SEED)
    weights = make_weights(rng)
    sizes = {}

    # Wejścia do numeryki: x std 1, zapis f16 wierszami (T x 4096).
    for t in SHAPES_T:
        x = rng.normal(0.0, 1.0, (t, D_MODEL)).astype(np.float16)
        x.tofile(os.path.join(out_dir, f"x_T{t}.f16.bin"))

    for rows, dim in TRIVIAL_SHAPES:
        name = f"trivial_{rows}x{dim}"
        sizes[name] = save_and_compile(convert(build_trivial_program(rows, dim)), out_dir, name)
    if "--trivial-only" in sys.argv:
        return

    for t in SHAPES_T:
        base = convert(build_ffn_program(t, weights, rank4=False))
        variants = {
            "fp16": base,
            "int4": quantize_int4(base),
            "lut4": palettize_lut4(base),
        }
        for vname, m in variants.items():
            sizes[f"ffn_T{t}_{vname}"] = save_and_compile(m, out_dir, f"ffn_T{t}_{vname}")
            if t == 512:
                # Wagi zdekwantyzowane do referencji cblas_sgemm (f32, układ [out, in]).
                for wname, arr in extract_dense_weights(m).items():
                    arr.tofile(os.path.join(out_dir, f"{wname}_{vname}.f32.bin"))

        # Wariant obrazowy (IOSurface): ten sam graf, wejście/wyjście jako obraz f16.
        img = set_image_io(quantize_int4(convert(build_ffn_program(t, weights, rank4=True))), t)
        sizes[f"ffn_img_T{t}_int4"] = save_and_compile(img, out_dir, f"ffn_img_T{t}_int4")
        # Ten sam graf rank-4, ale z wejściem MLMultiArray — para kontrolna dla (a) vs (b).
        arr4 = quantize_int4(convert(build_ffn_program(t, weights, rank4=True)))
        sizes[f"ffn_r4_T{t}_int4"] = save_and_compile(arr4, out_dir, f"ffn_r4_T{t}_int4")

    with open(os.path.join(out_dir, "sizes.txt"), "w") as f:
        for k, v in sizes.items():
            f.write(f"{k} {v}\n")
    log("gotowe")


A10_ENCODINGS = ["int4", "blockwise", "blockwise_zp", "affine16"]


def pkg_size(path):
    total = 0
    for root, _, files in os.walk(path):
        for f in files:
            total += os.path.getsize(os.path.join(root, f))
    return total


def main_a10():
    """EKS-A10 faza 0: modele do sond 0.1 (blockwise) i 0.2 (multifunction).
    Wagi i x te same co w A9 (ten sam SEED), więc pliki x_T*.f16.bin są zgodne."""
    out_dir = sys.argv[1]
    multi_variant = "blockwise"
    for a in sys.argv[2:]:
        if a.startswith("--multi-variant="):
            multi_variant = a.split("=", 1)[1]
    os.makedirs(out_dir, exist_ok=True)
    rng = np.random.default_rng(SEED)
    weights = make_weights(rng)
    sizes = {}
    for t in SHAPES_T:
        x = rng.normal(0.0, 1.0, (t, D_MODEL)).astype(np.float16)
        x.tofile(os.path.join(out_dir, f"x_T{t}.f16.bin"))

    # Sonda 0.1: różnica reprezentacji — dekwantyzacja coremltools (scale·(q−offset_f16))
    # wobec wzoru MLX (q·scale+bias) na tych samych q/scale/bias.
    for wname, w in zip(["w_gate", "w_up", "w_down"], weights):
        q, sc, b = quantize_affine_group(w)
        w_mlx = dequant_mlx(q, sc, b)
        off = (-b.astype(np.float32) / sc.astype(np.float32)).astype(np.float16)
        w_cml = np.repeat(sc.astype(np.float32), GROUP, axis=1) * (
            q.astype(np.float32) - np.repeat(off.astype(np.float32), GROUP, axis=1))
        rel = np.linalg.norm(w_cml - w_mlx) / np.linalg.norm(w_mlx)
        exact = np.mean(w_cml.astype(np.float16) == w_mlx.astype(np.float16)) * 100
        log(f"{wname}: kwantyzacja grupa 64 wobec f16: relL2 "
            f"{np.linalg.norm(w_mlx - w.astype(np.float32)) / np.linalg.norm(w.astype(np.float32)):.3e}; "
            f"offset f16 wobec wzoru MLX: relL2 {rel:.3e}, równych w f16 {exact:.2f}%")
        w_mlx.tofile(os.path.join(out_dir, f"{wname}_mlx.f32.bin"))

    for t in SHAPES_T:
        for enc in A10_ENCODINGS:
            if enc == "int4":
                m = quantize_int4(convert(build_ffn_program(t, weights, rank4=False)))
            else:
                m = convert(build_ffn_program(t, weights, rank4=False, encoding=enc))
            name = f"ffn_T{t}_{enc}"
            sizes[name] = save_and_compile(m, out_dir, name)
            sizes[name + ".mlpackage"] = pkg_size(os.path.join(out_dir, name + ".mlpackage"))
            if t == 512:
                for wname, arr in extract_dense_weights(m).items():
                    arr.tofile(os.path.join(out_dir, f"{wname}_{enc}.f32.bin"))

    # Sonda 0.2: jeden mlpackage z funkcjami T256/T512/T1024 na tych samych wagach.
    desc = MultiFunctionDescriptor()
    for t in SHAPES_T:
        desc.add_function(os.path.join(out_dir, f"ffn_T{t}_{multi_variant}.mlpackage"), "main", f"T{t}")
    desc.default_function_name = "T512"
    multi_pkg = os.path.join(out_dir, f"ffn_multi_{multi_variant}.mlpackage")
    if os.path.exists(multi_pkg):
        shutil.rmtree(multi_pkg)
    t0 = time.time()
    save_multifunction(desc, multi_pkg)
    log(f"save_multifunction: {time.time() - t0:.1f} s")
    t0 = time.time()
    subprocess.run(["xcrun", "coremlcompiler", "compile", multi_pkg, out_dir], check=True,
                   stdout=subprocess.DEVNULL)
    log(f"kompilacja multifunction: {time.time() - t0:.1f} s")
    sizes[f"ffn_multi_{multi_variant}.mlpackage"] = pkg_size(multi_pkg)
    sizes[f"ffn_multi_{multi_variant}"] = pkg_size(os.path.join(out_dir, f"ffn_multi_{multi_variant}.mlmodelc"))
    for t in SHAPES_T:
        n = f"ffn_T{t}_{multi_variant}"
        log(f"{n}: mlpackage {sizes[n + '.mlpackage'] / 2**20:.2f} MiB, mlmodelc {sizes[n] / 2**20:.2f} MiB")
    log(f"multifunction: mlpackage {sizes[f'ffn_multi_{multi_variant}.mlpackage'] / 2**20:.2f} MiB, "
        f"mlmodelc {sizes[f'ffn_multi_{multi_variant}'] / 2**20:.2f} MiB")

    with open(os.path.join(out_dir, "sizes.txt"), "w") as f:
        for k, v in sizes.items():
            f.write(f"{k} {v}\n")
    log("gotowe")


if __name__ == "__main__":
    if "--a10" in sys.argv:
        main_a10()
    else:
        main()
