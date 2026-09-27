#!/usr/bin/env python3
# =============================================================================
# Plik: ane_export.py
# Opis: Eksport wycinków wag (ogona wierszy projekcji) z checkpointu MLX 4-bit
#       do skompilowanych modeli CoreML (.mlmodelc) dla ANE. Kodowanie główne
#       per_channel_int8: re-kwantyzacja symetryczna na wiersz (ANE 100%,
#       raport fazy 0). Kodowanie blockwise (uint4 + skala/offset f16 na grupę
#       64) jest bit-exact z checkpointem, ale CoreML liczy je na CPU — tylko
#       do kontroli. Pisze manifest.json z opisem wycinków i wynikiem verify.
# Przykład: nice -n 10 python ane_export.py <checkpoint> <out_dir> \
#             --groups gate_up,down --shapes 256,512,1024 \
#             --encoding per_channel_int8 --multifunction --verify
# =============================================================================

import argparse
import concurrent.futures
import glob
import hashlib
import json
import os
import shutil
import subprocess
import sys
import time

import numpy as np

VERSION = 1
GROUP_SIZE = 64          # grupa kwantyzacji MLX (scale/bias na 64 kolejne wagi)
NIBBLES_PER_WORD = 8     # osiem wag 4-bitowych w słowie uint32
ROW_ALIGN = 64           # wycinek ANE zaokrąglany w dół do wielokrotności 64 wierszy
PREFILL_SLOT = 1024      # wierszy w slocie prefillu wykonawcy; każde T musi go dzielić
HEAD_HASH_BYTES = 64 * 2**20

# Grupa -> lista projekcji dzielących to samo wejście x.
GROUPS = {
    "gate_up": ["gate", "up"],
    "down": ["down"],
    "qkv": ["q", "k", "v"],
    "o": ["o"],
}
PROJ_TENSOR = {
    "q": "self_attn.q_proj", "k": "self_attn.k_proj", "v": "self_attn.v_proj",
    "o": "self_attn.o_proj",
    "gate": "mlp.gate_proj", "up": "mlp.up_proj", "down": "mlp.down_proj",
}


def log(msg):
    print(f"[ane-export] {msg}", flush=True)


# -----------------------------------------------------------------------------
# Safetensors bez torch: własny parser nagłówka + memmap.
# -----------------------------------------------------------------------------
class SafeTensors:
    def __init__(self, path):
        self.path = path
        with open(path, "rb") as f:
            n = int.from_bytes(f.read(8), "little")
            self.header = json.loads(f.read(n))
        self.data_off = 8 + n
        self.header.pop("__metadata__", None)
        self.mm = np.memmap(path, dtype=np.uint8, mode="r")

    def raw(self, name):
        """Widok memmap na tensor (bf16 jako uint16, u32 jako uint32)."""
        e = self.header[name]
        a, b = e["data_offsets"]
        dt = {"U32": np.uint32, "BF16": np.uint16, "F16": np.float16, "F32": np.float32}[e["dtype"]]
        buf = self.mm[self.data_off + a:self.data_off + b]
        return buf.view(dt).reshape(e["shape"]), e["dtype"]

    def f32(self, name):
        arr, dtype = self.raw(name)
        if dtype == "BF16":
            return (arr.astype(np.uint32) << 16).view(np.float32)
        return arr.astype(np.float32)

    def layer_count(self):
        n = 0
        while f"model.layers.{n}.mlp.gate_proj.weight" in self.header:
            n += 1
        return n


def resolve_checkpoint(p):
    if os.path.isdir(p):
        cands = sorted(glob.glob(os.path.join(p, "*.safetensors")))
        if len(cands) != 1:
            sys.exit(f"oczekiwano jednego pliku .safetensors w {p}, jest {len(cands)}")
        return cands[0]
    return p


def source_name(path):
    """Nazwa checkpointu: katalog models--* z cache HF albo katalog nadrzędny pliku."""
    for part in reversed(os.path.abspath(path).split(os.sep)):
        if part.startswith("models--"):
            return part
    return os.path.basename(os.path.dirname(os.path.abspath(path)))


def head_sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        h.update(f.read(HEAD_HASH_BYTES))
    return h.hexdigest()


# -----------------------------------------------------------------------------
# Wycinek projekcji: ogon wierszy [ane0, rows) w postaci q/scale/bias.
# -----------------------------------------------------------------------------
def ane_split(rows, share):
    ane_rows = int(rows * share) // ROW_ALIGN * ROW_ALIGN
    if ane_rows <= 0:
        sys.exit(f"udział {share} z {rows} wierszy daje pusty ogon ANE (ane_rows = 0)")
    return rows - ane_rows, ane_rows


def load_slice(st, layer, proj, share):
    """Zwraca dict: q uint8 [a, K] (0..15), scale/bias f32 [a, K/64], rows, cols, ane0."""
    base = f"model.layers.{layer}.{PROJ_TENSOR[proj]}"
    packed, _ = st.raw(base + ".weight")
    rows, words = packed.shape
    cols = words * NIBBLES_PER_WORD
    ane0, ane_rows = ane_split(rows, share)
    w = np.ascontiguousarray(packed[ane0:])                      # [a, cols/8] u32
    # Najmłodsze bity słowa = najwcześniejsza waga (jak w forge-formats/mlx.rs).
    shifts = np.arange(NIBBLES_PER_WORD, dtype=np.uint32) * 4
    q = ((w[:, :, None] >> shifts) & 0xF).astype(np.uint8).reshape(ane_rows, cols)
    scale = st.f32(base + ".scales")[ane0:]
    bias = st.f32(base + ".biases")[ane0:]
    assert scale.shape == (ane_rows, cols // GROUP_SIZE), scale.shape
    return dict(proj=proj, q=q, scale=scale, bias=bias, rows=rows, cols=cols,
                ane0=ane0, ane_rows=ane_rows)


def dequant_f32(part):
    s = np.repeat(part["scale"], GROUP_SIZE, axis=1)
    b = np.repeat(part["bias"], GROUP_SIZE, axis=1)
    return part["q"].astype(np.float32) * s + b


def to_f16_checked(arr, what):
    """bf16->f16: dokładne, gdy wykładnik mieści się w f16; liczy niedokładne."""
    out = arr.astype(np.float16)
    bad = int(np.count_nonzero(out.astype(np.float32) != arr))
    if bad:
        log(f"UWAGA: {what}: {bad} wartości niedokładnych po konwersji do f16")
    return out


def int8_per_channel(w32):
    """Symetryczna kwantyzacja per wiersz wyjściowy: scale = max|w|/127, q = round(w/scale).
    Skala jest zaokrąglana do f16 PRZED liczeniem q — model trzyma ją w f16, więc q
    liczone względem skali f32 dawałoby inne wagi niż te, które ANE faktycznie
    dekwantyzuje. Zwraca skalę już jako f16 (tę samą, którą dostaje model i verify)."""
    amax = np.abs(w32).max(axis=1, keepdims=True)
    scale16 = np.where(amax == 0, 1.0, amax / 127.0).astype(np.float16)
    scale = scale16.astype(np.float32)
    q = np.clip(np.rint(w32 / scale), -127, 127).astype(np.int8)
    return q, scale16.reshape(-1)


def blockwise_params(part):
    """Parametry constexpr_blockwise_shift_scale: out = scale*(q - offset),
    więc offset = -bias/scale daje w = q*scale + bias."""
    scale = part["scale"]
    offset = np.where(scale != 0, -part["bias"] / np.where(scale != 0, scale, 1.0), 0.0)
    return to_f16_checked(scale, f"{part['proj']} scale"), offset.astype(np.float16)


# -----------------------------------------------------------------------------
# Budowa programu MIL i konwersja.
# -----------------------------------------------------------------------------
def build_program(parts, t_rows, encoding):
    import coremltools as ct
    from coremltools.converters.mil import Builder as mb
    from coremltools.converters.mil.mil import types

    k = parts[0]["cols"]
    consts = []
    for p in parts:
        if encoding == "blockwise":
            scale, offset = blockwise_params(p)
            consts.append(("blockwise", p["q"].astype(types.np_uint4_dtype), scale, offset))
        elif encoding == "per_channel_int8":
            q, scale = int8_per_channel(dequant_f32(p))
            consts.append(("int8", q, scale, None))
        else:
            consts.append(("dense", dequant_f32(p).astype(np.float16), None, None))

    @mb.program(input_specs=[mb.TensorSpec(shape=(t_rows, k), dtype=types.fp16)],
                opset_version=ct.target.macOS15)
    def prog(x):
        outs = []
        for p, (kind, data, scale, offset) in zip(parts, consts):
            name = f"w_{p['proj']}"
            if kind == "blockwise":
                w = mb.constexpr_blockwise_shift_scale(data=data, scale=scale, offset=offset,
                                                       name=name)
            elif kind == "int8":
                # iOS16: w = scale * (q - zero_point), skala na kanał wyjściowy (axis=0),
                # zero_point 0 — jedyna postać, którą kompilator ANE zostawia na ANE.
                w = mb.constexpr_affine_dequantize(
                    quantized_data=data, zero_point=np.zeros((data.shape[0],), dtype=np.int8),
                    scale=scale, axis=0, name=name)
            else:
                w = mb.const(val=data, name=name)
            outs.append(mb.linear(x=x, weight=w, name=f"y_{p['proj']}"))
        y = outs[0] if len(outs) == 1 else mb.concat(values=outs, axis=1)
        return mb.identity(x=y, name="y")

    return prog


def convert(prog, encoding):
    import coremltools as ct
    m = ct.convert(prog, source="milinternal", convert_to="mlprogram",
                   compute_precision=ct.precision.FLOAT16,
                   minimum_deployment_target=ct.target.macOS15,
                   skip_model_load=True)
    if encoding == "per_channel":
        from coremltools.optimize.coreml import (OpLinearQuantizerConfig, OptimizationConfig,
                                                 linear_quantize_weights)
        cfg = OptimizationConfig(global_config=OpLinearQuantizerConfig(
            mode="linear_symmetric", dtype="int4", granularity="per_channel"))
        m = linear_quantize_weights(m, cfg)
    return m


def compute_plan_summary(mlmodelc):
    """Przydział operacji przez MLComputePlan (CPU_AND_NE) dla każdej funkcji programu.
    Zwraca {funkcja: {"linear": {"ANE": n, ...}, "other": {...}, "ane_cost_pct": p}}."""
    import coremltools as ct
    from coremltools.models.compute_plan import MLComputePlan
    plan = MLComputePlan.load_from_path(mlmodelc, ct.ComputeUnit.CPU_AND_NE)
    prog = plan.model_structure.program
    out = {}
    for fname, fn in prog.functions.items():
        lin, other = {}, {}
        cost_all = cost_ane = 0.0
        for op in fn.block.operations:
            opname = op.operator_name.split(".")[-1]          # "ios18.linear" -> "linear"
            if opname == "const" or opname.startswith("constexpr_"):
                continue
            usage = plan.get_compute_device_usage_for_mlprogram_operation(op)
            cost = plan.get_estimated_cost_for_mlprogram_operation(op)
            c = cost.weight if cost is not None else 0.0
            cost_all += c
            dev = "brak"
            if usage is not None:
                d = usage.preferred_compute_device
                dev = type(d).__name__.replace("ML", "").replace("ComputeDevice", "")
                if dev == "NeuralEngine":
                    dev = "ANE"
                    cost_ane += c
            bucket = lin if opname == "linear" else other
            bucket[dev] = bucket.get(dev, 0) + 1
        out[fname] = dict(linear=lin, other=other,
                          ane_cost_pct=round(100 * cost_ane / cost_all, 1) if cost_all else None)
    return out


def compile_pkg(pkg, out_dir):
    name = os.path.splitext(os.path.basename(pkg))[0]
    dst = os.path.join(out_dir, name + ".mlmodelc")
    if os.path.exists(dst):
        shutil.rmtree(dst)
    subprocess.run(["xcrun", "coremlcompiler", "compile", pkg, out_dir], check=True,
                   stdout=subprocess.DEVNULL)
    return dst, dir_size(dst)


def dir_size(d):
    return sum(os.path.getsize(os.path.join(r, f)) for r, _, fs in os.walk(d) for f in fs)


# -----------------------------------------------------------------------------
# Jedno zadanie: (warstwa, grupa) -> model(e) + wpis manifestu.
# -----------------------------------------------------------------------------
def export_job(args):
    ckpt, out_dir, layer, group, share, shapes, encoding, layout, keep_pkg, plan_check = args
    import coremltools as ct
    t_start = time.time()
    st = SafeTensors(ckpt)
    parts = [load_slice(st, layer, proj, share) for proj in GROUPS[group]]
    tag = f"L{layer:02d}_{group}"
    entry = dict(layer=layer, group=group, functions={}, parts=[],
                 input=dict(name="x", cols=parts[0]["cols"]),
                 output=dict(name="y", width=sum(p["ane_rows"] for p in parts)))
    col0 = 0
    for p in parts:
        entry["parts"].append(dict(proj=p["proj"], rows=p["rows"], cols=p["cols"],
                                   ane0=p["ane0"], ane_rows=p["ane_rows"], out_col0=col0))
        col0 += p["ane_rows"]

    pkg_dir = os.path.join(out_dir, "mlpackage")
    os.makedirs(pkg_dir, exist_ok=True)
    timings = {}
    sizes = {}
    per_t_pkgs = {}
    for t in shapes:
        t0 = time.time()
        m = convert(build_program(parts, t, encoding), encoding)
        pkg = os.path.join(pkg_dir, f"{tag}_T{t}.mlpackage")
        if os.path.exists(pkg):
            shutil.rmtree(pkg)
        m.save(pkg)
        per_t_pkgs[t] = pkg
        timings[f"convert_T{t}"] = round(time.time() - t0, 2)

    if layout == "multifunction":
        from coremltools.models.utils import MultiFunctionDescriptor, save_multifunction
        t0 = time.time()
        desc = MultiFunctionDescriptor()
        for t, pkg in per_t_pkgs.items():
            desc.add_function(pkg, "main", f"T{t}")
            entry["functions"][str(t)] = f"T{t}"
        desc.default_function_name = f"T{shapes[0]}"
        mf_pkg = os.path.join(pkg_dir, f"{tag}.mlpackage")
        if os.path.exists(mf_pkg):
            shutil.rmtree(mf_pkg)
        save_multifunction(desc, mf_pkg)
        timings["multifunction"] = round(time.time() - t0, 2)
        t0 = time.time()
        mlmodelc, size = compile_pkg(mf_pkg, out_dir)
        timings["compile"] = round(time.time() - t0, 2)
        entry["model"] = os.path.basename(mlmodelc)
        sizes[os.path.basename(mlmodelc)] = size
        if plan_check:
            # Python MLComputePlan nie wybiera funkcji (przydział widać tylko dla
            # domyślnej), więc plan liczony jest na jednofunkcyjnych kopiach per T
            # (raport fazy 0 §0.2: funkcje multifunction liczą identycznie).
            t0 = time.time()
            tmp = os.path.join(pkg_dir, f"plan_{tag}")
            os.makedirs(tmp, exist_ok=True)
            entry["compute_plan"] = {}
            for t, pkg in per_t_pkgs.items():
                c, _ = compile_pkg(pkg, tmp)
                entry["compute_plan"][f"T{t}"] = compute_plan_summary(c)["main"]
            shutil.rmtree(tmp, ignore_errors=True)
            timings["plan"] = round(time.time() - t0, 2)
    else:
        entry["models"] = {}
        t0 = time.time()
        for t, pkg in per_t_pkgs.items():
            mlmodelc, size = compile_pkg(pkg, out_dir)
            entry["models"][str(t)] = os.path.basename(mlmodelc)
            entry["functions"][str(t)] = "main"
            sizes[os.path.basename(mlmodelc)] = size
            if plan_check:
                entry.setdefault("compute_plan", {})[f"T{t}"] = compute_plan_summary(mlmodelc)["main"]
        timings["compile"] = round(time.time() - t0, 2)

    if not keep_pkg:
        for pkg in per_t_pkgs.values():
            shutil.rmtree(pkg, ignore_errors=True)
        if layout == "multifunction":
            shutil.rmtree(mf_pkg, ignore_errors=True)
    timings["total"] = round(time.time() - t_start, 2)
    plan_txt = f" plan {entry['compute_plan']}" if plan_check else ""
    log(f"{tag}: {timings} rozmiar {sum(sizes.values()) / 2**20:.1f} MiB{plan_txt}")
    return entry, timings, sizes, per_t_pkgs


# -----------------------------------------------------------------------------
# Weryfikacja numeryczna (warstwa pierwsza, każda grupa, najmniejsze T).
# -----------------------------------------------------------------------------
def rel_l2(a, b, eps=1e-12):
    """Względny błąd L2; mianownik nie schodzi poniżej eps (zerowe b = brak dzielenia przez 0)."""
    return float(np.linalg.norm(a - b) / max(float(np.linalg.norm(b)), eps))


def verify_group(ckpt, out_dir, layer, group, share, t, encoding, seed=20260911):
    """Buduje jednofunkcyjny model T dla (warstwa, grupa), liczy błąd wyjścia wobec
    dekwantyzacji MLX grupa-64 (f32) na x ~ N(0,1) i x ~ Student-t(3), błąd samych
    wag oraz (blockwise) bit-exactness decompress_weights."""
    import coremltools as ct
    from coremltools.optimize.coreml import decompress_weights
    st = SafeTensors(ckpt)
    parts = [load_slice(st, layer, proj, share) for proj in GROUPS[group]]
    w_ref = np.concatenate([dequant_f32(p) for p in parts], axis=0)      # [sum a, K]
    k = parts[0]["cols"]
    rng = np.random.default_rng(seed)
    xs = {"gauss": rng.normal(0.0, 1.0, (t, k)).astype(np.float16),
          "student_t3": rng.standard_t(3, (t, k)).astype(np.float16)}
    rep = dict(layer=layer, group=group, T=t, encoding=encoding,
               output_width=int(w_ref.shape[0]), weights={}, outputs={})

    # Błąd samych wag: to, co model faktycznie zdekwantyzuje (numpy), wobec MLX.
    if encoding == "per_channel_int8":
        w_model = np.concatenate([(lambda q, sc: q.astype(np.float32) * sc.astype(np.float32)[:, None])
                                  (*int8_per_channel(dequant_f32(p))) for p in parts], axis=0)
    elif encoding == "blockwise":
        w_model = np.concatenate([
            (lambda sc, off: np.repeat(sc.astype(np.float32), GROUP_SIZE, 1)
             * (p["q"].astype(np.float32) - np.repeat(off.astype(np.float32), GROUP_SIZE, 1)))
            (*blockwise_params(p)) for p in parts], axis=0)
    else:
        w_model = None
    if w_model is not None:
        d = np.abs(w_model - w_ref)
        rep["weights"] = dict(rel_l2=rel_l2(w_model, w_ref), max_abs=float(d.max()),
                              mean_abs=float(d.mean()), w_max_abs=float(np.abs(w_ref).max()))

    pkg = os.path.join(out_dir, "mlpackage", f"verify_L{layer:02d}_{group}_T{t}.mlpackage")
    os.makedirs(os.path.dirname(pkg), exist_ok=True)
    if os.path.exists(pkg):
        shutil.rmtree(pkg)
    convert(build_program(parts, t, encoding), encoding).save(pkg)
    # CPU_ONLY: kernel linear akumuluje w fp16 (rel_L2 ~6e-3 dla K=4096 niezależnie od
    # compute_precision, także dla gęstej stałej f16) — dlatego jest też GPU jako
    # kontrola i czysty błąd kwantyzacji liczony w numpy (f32). ANE celowo pominięte.
    models = {}
    for label, cu in (("cpu", ct.ComputeUnit.CPU_ONLY), ("gpu", ct.ComputeUnit.CPU_AND_GPU)):
        try:
            models[label] = ct.models.MLModel(pkg, compute_units=cu)
        except Exception as e:  # noqa: BLE001
            rep["outputs"][label] = f"niedostępne: {e!r}"
    for xname, x in xs.items():
        y_ref = x.astype(np.float32) @ w_ref.T
        r = dict(y_max_abs=float(np.abs(y_ref).max()))
        if w_model is not None:
            y_np = x.astype(np.float32) @ w_model.T
            r["numpy_f32"] = dict(rel_l2=rel_l2(y_np, y_ref), max_abs=float(np.abs(y_np - y_ref).max()))
        for label, m in models.items():
            y = np.asarray(m.predict({"x": x})["y"], dtype=np.float32)
            r[label] = dict(rel_l2=rel_l2(y, y_ref), max_abs=float(np.abs(y - y_ref).max()))
        rep["outputs"][xname] = r

    if encoding == "blockwise" and "cpu" in models:
        try:
            dense = decompress_weights(models["cpu"])
            consts = {op.name: np.asarray(op.outputs[0].val, dtype=np.float32)
                      for op in dense._mil_program.functions["main"].operations
                      if op.op_type == "const" and op.outputs[0].val is not None
                      and np.asarray(op.outputs[0].val).ndim == 2}
            dec = {}
            for p in parts:
                cand = [v for n, v in consts.items() if f"w_{p['proj']}" in n
                        and v.shape == (p["ane_rows"], p["cols"])]
                if not cand:
                    dec[p["proj"]] = "brak stałej po dekompresji"
                    continue
                d = np.abs(cand[0] - dequant_f32(p))
                dec[p["proj"]] = dict(max_abs=float(d.max()), mean_abs=float(d.mean()))
            rep["decompress"] = dec
        except Exception as e:  # noqa: BLE001
            rep["decompress"] = f"decompress_weights nie działa: {e!r}"
    shutil.rmtree(pkg, ignore_errors=True)
    return rep


# -----------------------------------------------------------------------------
def parse_layers(spec, n):
    if spec is None:
        return list(range(n))
    out = []
    for tok in spec.split(","):
        if "-" in tok:
            a, b = tok.split("-")
            out.extend(range(int(a), int(b) + 1))
        else:
            out.append(int(tok))
    bad = [l for l in out if l < 0 or l >= n]
    if bad:
        sys.exit(f"warstwy {bad} poza zakresem 0..{n - 1}")
    return out


def main():
    ap = argparse.ArgumentParser(description="Eksport wycinków wag MLX 4-bit do CoreML/ANE")
    ap.add_argument("checkpoint")
    ap.add_argument("out_dir")
    ap.add_argument("--share", type=float, default=0.6)
    ap.add_argument("--shapes", default="256,512,1024")
    ap.add_argument("--groups", default="gate_up,down")
    ap.add_argument("--encoding", choices=["per_channel_int8", "blockwise", "per_channel"],
                    default="per_channel_int8")
    ap.add_argument("--layers", default=None, help="np. 0-31 lub 0,5,7")
    ap.add_argument("--workers", type=int, default=1)
    lay = ap.add_mutually_exclusive_group()
    lay.add_argument("--multifunction", action="store_true", default=True)
    lay.add_argument("--per-shape", action="store_true")
    ap.add_argument("--verify", action="store_true")
    ap.add_argument("--verify-layers", default="0,20,39",
                    help="warstwy do kontroli numerycznej (poza zakresem checkpointu pomijane)")
    ap.add_argument("--no-plan", action="store_true", help="pomiń MLComputePlan po kompilacji")
    ap.add_argument("--keep-mlpackage", action="store_true")
    a = ap.parse_args()

    ckpt = resolve_checkpoint(a.checkpoint)
    if not 0.0 < a.share < 1.0:
        sys.exit(f"--share musi być w (0, 1), jest {a.share}")
    shapes = [int(s) for s in a.shapes.split(",")]
    # Rosnące i dzielące slot prefillu (1024): wykonawca liczy większy kafel
    # kawałkami mniejszego T, więc T musi dzielić slot, żeby ostatni kawałek
    # nie wyszedł poza niego.
    if shapes != sorted(set(shapes)):
        sys.exit(f"--shapes muszą rosnąć bez powtórzeń, są {shapes}")
    bad = [t for t in shapes if t <= 0 or PREFILL_SLOT % t != 0]
    if bad:
        sys.exit(f"--shapes {bad} nie dzielą slotu prefillu {PREFILL_SLOT}")
    groups = a.groups.split(",")
    for g in groups:
        if g not in GROUPS:
            sys.exit(f"nieznana grupa {g}; dostępne: {list(GROUPS)}")
    layout = "per_shape" if a.per_shape else "multifunction"
    st = SafeTensors(ckpt)
    n_layers = st.layer_count()
    layers = parse_layers(a.layers, n_layers)
    d_model = st.header["model.layers.0.mlp.gate_proj.weight"]["shape"][1] * NIBBLES_PER_WORD
    inter = st.header["model.layers.0.mlp.gate_proj.weight"]["shape"][0]
    log(f"checkpoint {ckpt}: {n_layers} warstw, d_model {d_model}, inter {inter}")
    os.makedirs(a.out_dir, exist_ok=True)
    keep_pkg = a.keep_mlpackage

    jobs = [(ckpt, a.out_dir, l, g, a.share, shapes, a.encoding, layout, keep_pkg, not a.no_plan)
            for l in layers for g in groups]
    t0 = time.time()
    if a.workers > 1:
        ctx = concurrent.futures.ProcessPoolExecutor(max_workers=a.workers)
        results = list(ctx.map(export_job, jobs))
    else:
        results = [export_job(j) for j in jobs]
    export_s = time.time() - t0

    entries, timings, sizes, pkgs = [], {}, {}, {}
    for (entry, tm, sz, per_t), job in zip(results, jobs):
        entries.append(entry)
        timings[f"L{job[2]:02d}_{job[3]}"] = tm
        sizes.update(sz)
        pkgs[(job[2], job[3])] = per_t

    manifest = dict(
        version=VERSION,
        source=source_name(ckpt),
        source_file=os.path.basename(ckpt),
        source_size_bytes=os.path.getsize(ckpt),
        source_head64mb_sha256=head_sha256(ckpt),
        encoding=a.encoding, share=a.share, shapes=shapes,
        d_model=d_model, inter=inter, group_size=GROUP_SIZE,
        n_layers_in_checkpoint=n_layers, layers=layers, layout=layout,
        input_dtype="fp16", output_dtype="fp16",
        groups=entries,
        sizes_bytes=sizes,
        timings_s=timings,
        export_wall_s=round(export_s, 2),
    )

    if a.verify:
        t_ver = min(shapes)
        v_layers = [l for l in (int(x) for x in a.verify_layers.split(",")) if 0 <= l < n_layers]
        reports = []
        t0 = time.time()
        for l in v_layers:
            for g in groups:
                rep = verify_group(ckpt, a.out_dir, l, g, a.share, t_ver, a.encoding)
                o = rep["outputs"]
                fmt = lambda r: " ".join(f"{k} {v['rel_l2']:.2e}" for k, v in r.items()
                                         if isinstance(v, dict) and "rel_l2" in v)
                log(f"verify L{l:02d}_{g} T{t_ver}: wagi rel_L2 "
                    f"{rep['weights'].get('rel_l2', float('nan')):.2e} "
                    f"max|dw| {rep['weights'].get('max_abs', float('nan')):.2e} | "
                    f"gauss: {fmt(o['gauss'])} | student_t3: {fmt(o['student_t3'])}"
                    + (f" | decompress {rep['decompress']}" if "decompress" in rep else ""))
                reports.append(rep)
        manifest["verify"] = reports
        manifest["verify_wall_s"] = round(time.time() - t0, 2)
        if not a.keep_mlpackage:
            shutil.rmtree(os.path.join(a.out_dir, "mlpackage"), ignore_errors=True)

    with open(os.path.join(a.out_dir, "manifest.json"), "w") as f:
        json.dump(manifest, f, indent=2)
    total = sum(sizes.values())
    log(f"gotowe: {len(entries)} modeli, {total / 2**20:.1f} MiB, eksport {export_s:.1f} s")


if __name__ == "__main__":
    main()
