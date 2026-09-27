#!/usr/bin/env python3
# =============================================================================
# Plik: eks_a10_probe_gen.py
# Opis: Sondy kodowania wag dla EKS-A10 faza 0 — buduje przy T=512 kilkanaście
#       wariantów kompresji (grupy różnej wielkości i osi, offset f16/uint4,
#       LUT per-grupa, int8 z grupą, matmul zamiast linear, rozbicie K na kawałki,
#       re-kodowanie wag MLX do int8/int4 per-channel) i dopisuje ich nazwy do
#       probe.txt, który czyta sekcja `probe` harnessu eks_a10_ane.swift.
# Przykład: venv/bin/python eks_a10_probe_gen.py /sciezka/na/modele_a10
#           venv/bin/python eks_a10_probe_gen.py /sciezka/na/modele_a10 --round2
# =============================================================================

import os
import sys
import numpy as np
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import eks_a9_gen as g
from eks_a9_gen import mb, types, ct, np_uint4_dtype, D_MODEL, N_SLICE
out = sys.argv[1]
rng = np.random.default_rng(g.SEED)
weights = g.make_weights(rng)
T = 512

def bw(w, name, group, offset=True, axis=1):
    """blockwise shift-scale, grupa `group` wzdłuż osi `axis` (1 = K, 0 = N)."""
    rows, k = w.shape
    if axis == 1:
        q, sc, b = g.quantize_affine_group(w, group)
    else:
        qT, scT, bT = g.quantize_affine_group(np.ascontiguousarray(w.T), group)
        q, sc, b = qT.T, scT.T, bT.T
    q = np.ascontiguousarray(q); sc = np.ascontiguousarray(sc); b = np.ascontiguousarray(b)
    kw = dict(data=q.astype(np_uint4_dtype), scale=sc, name=name)
    if offset:
        kw["offset"] = (-b.astype(np.float32) / sc.astype(np.float32)).astype(np.float16)
    return mb.constexpr_blockwise_shift_scale(**kw)

def lut_group(w, name, group=64):
    """LUT 4-bit per grupa 64 wzdłuż K: lut[n, g, :] = f16(q*scale+bias) — bit-exact wobec MLX."""
    q, sc, b = g.quantize_affine_group(w, group)
    rows, k = w.shape
    qs = np.arange(16, dtype=np.float32)
    lut = (qs[None, None, :] * sc.astype(np.float32)[:, :, None] + b.astype(np.float32)[:, :, None]).astype(np.float16)
    lut = lut.reshape(rows, k // group, 16, 1)
    return mb.constexpr_lut_to_dense(indices=q.astype(np_uint4_dtype), lut=lut, name=name)

def lut_group_rows(w, name, group=64):
    """LUT per grupa 64 wzdłuż N (kontrola osi)."""
    q, sc, b = g.quantize_affine_group(np.ascontiguousarray(w.T), group)
    q, sc, b = np.ascontiguousarray(q.T), np.ascontiguousarray(sc.T), np.ascontiguousarray(b.T)
    rows, k = w.shape
    qs = np.arange(16, dtype=np.float32)
    lut = (qs[None, None, :] * sc.astype(np.float32)[:, :, None] + b.astype(np.float32)[:, :, None]).astype(np.float16)
    lut = lut.reshape(rows // group, k, 16, 1)
    return mb.constexpr_lut_to_dense(indices=q.astype(np_uint4_dtype), lut=lut, name=name)

def int8_pc_of_mlx(w, name):
    """int8 per-channel policzone z wag ZDEKWANTYZOWANYCH grupa 64 (re-kodowanie MLX → ANE)."""
    q, sc, b = g.quantize_affine_group(w)
    w_mlx = g.dequant_mlx(q, sc, b)
    amax = np.abs(w_mlx).max(axis=1, keepdims=True)
    scale = np.where(amax == 0, 1e-7, amax / 127.0)
    q8 = np.clip(np.round(w_mlx / scale), -128, 127).astype(np.int8)
    return mb.constexpr_blockwise_shift_scale(data=q8, scale=scale.astype(np.float16), name=name)

def int4_pc_of_mlx(w, name):
    """int4 per-channel symetryczne z wag zdekwantyzowanych grupa 64."""
    q, sc, b = g.quantize_affine_group(w)
    w_mlx = g.dequant_mlx(q, sc, b)
    amax = np.abs(w_mlx).max(axis=1, keepdims=True)
    scale = np.where(amax == 0, 1e-7, amax / 7.0)
    q4 = np.clip(np.round(w_mlx / scale), -8, 7).astype(np.int8)
    return mb.constexpr_blockwise_shift_scale(data=q4.astype(types.nptype_from_builtin(types.int4)),
                                              scale=scale.astype(np.float16), name=name)

def prog_generic(wconst, split=None, matmul=False):
    w_gate, w_up, w_down = weights
    @mb.program(input_specs=[mb.TensorSpec(shape=(T, D_MODEL), dtype=types.fp16)], opset_version=ct.target.macOS15)
    def prog(x):
        def lin(h, w, name):
            if matmul:
                return mb.matmul(x=h, y=wconst(np.ascontiguousarray(w.T), name), name=name + "_mm")
            if split:
                rows, k = w.shape
                acc = None
                for gi in range(k // split):
                    hs = mb.slice_by_index(x=h, begin=[0, gi * split], end=[0, (gi + 1) * split],
                                           begin_mask=[True, False], end_mask=[True, False])
                    part = mb.linear(x=hs, weight=wconst(np.ascontiguousarray(w[:, gi * split:(gi + 1) * split]), f"{name}_w{gi}"))
                    acc = part if acc is None else mb.add(x=acc, y=part)
                return acc
            return mb.linear(x=h, weight=wconst(w, name), name=name)
        gate = lin(x, w_gate, "w_gate")
        up = lin(x, w_up, "w_up")
        act = mb.mul(x=mb.silu(x=gate), y=up, name="act")
        y = lin(act, w_down, "w_down")
        return mb.identity(x=y, name="y")
    return prog

def pc_split(w, name):
    """per-channel z offsetem f16 dla kawałka [N, 64] — składnik wariantu split64."""
    q, sc, b = g.quantize_affine_group(w, w.shape[1])
    off = (-b.astype(np.float32) / sc.astype(np.float32)).astype(np.float16)
    return mb.constexpr_blockwise_shift_scale(data=q.astype(np_uint4_dtype), scale=sc, offset=off, name=name)

variants = {
    "pc_off16":      prog_generic(lambda w, n: bw(w, n, D_MODEL if w.shape[1] == D_MODEL else N_SLICE, offset=True)),
    "g64_nooff":     prog_generic(lambda w, n: bw(w, n, 64, offset=False)),
    "g128":          prog_generic(lambda w, n: bw(w, n, 128)),
    "g256":          prog_generic(lambda w, n: bw(w, n, 256)),
    "g512":          prog_generic(lambda w, n: bw(w, n, 512)),
    "g1024":         prog_generic(lambda w, n: bw(w, n, 1024)),
    "g1536":         prog_generic(lambda w, n: bw(w, n, 1536 if w.shape[1] == N_SLICE else 2048)),
    "g64_axisN":     prog_generic(lambda w, n: bw(w, n, 64, axis=0)),
    "g64_matmul":    prog_generic(lambda w, n: bw(w, n, 64, axis=0), matmul=True),  # W^T: grupa wzdłuż K = oś 0 W^T
    "lut_g64":       prog_generic(lambda w, n: lut_group(w, n, 64)),
    "lut_g64_axisN": prog_generic(lambda w, n: lut_group_rows(w, n, 64)),
    "int8pc_mlx":    prog_generic(int8_pc_of_mlx),
    "int4pc_mlx":    prog_generic(int4_pc_of_mlx),
    "split64":       prog_generic(pc_split, split=64),
    "split512":      prog_generic(pc_split, split=512),
}
only = [a for a in sys.argv[2:] if not a.startswith("--")]
if "--round2" in sys.argv: only = ["__none__"]
names = []
for name, prog in variants.items():
    if only and name not in only:
        continue
    try:
        m = g.convert(prog)
        g.save_and_compile(m, out, f"probe_{name}")
        names.append(f"probe_{name}")
        if name in ("int8pc_mlx", "int4pc_mlx", "lut_g64"):
            for wname, arr in g.extract_dense_weights(m).items():
                arr.tofile(os.path.join(out, f"{wname}_{name}.f32.bin"))
    except Exception as e:
        print(f"[probe] {name}: BŁĄD {type(e).__name__}: {str(e)[:300]}", flush=True)
with open(os.path.join(out, "probe.txt"), "a") as f:
    f.write("\n".join(names) + "\n")

# --- druga tura sond: per-channel z zero-pointem uint4; grupa 64 w int8 ---
def pc_zp4(w, name):
    rows, k = w.shape
    w32 = w.astype(np.float32)
    wmin = w32.min(axis=1, keepdims=True); wmax = w32.max(axis=1, keepdims=True)
    scale = np.where(wmax - wmin == 0, 1e-7, (wmax - wmin) / 15.0)
    z = np.clip(np.round(-wmin / scale), 0, 15)
    q = np.clip(np.round(w32 / scale) + z, 0, 15).astype(np.uint8)
    return mb.constexpr_blockwise_shift_scale(data=q.astype(np_uint4_dtype), scale=scale.astype(np.float16),
                                              offset=z.astype(np.uint8).astype(np_uint4_dtype), name=name)

def g64_int8(w, name):
    rows, k = w.shape
    w32 = w.astype(np.float32).reshape(rows, k // 64, 64)
    amax = np.abs(w32).max(axis=2, keepdims=True)
    scale = np.where(amax == 0, 1e-7, amax / 127.0)
    q = np.clip(np.round(w32 / scale), -128, 127).astype(np.int8).reshape(rows, k)
    return mb.constexpr_blockwise_shift_scale(data=q, scale=scale.astype(np.float16).reshape(rows, k // 64), name=name)

if "--round2" in sys.argv:
    names = []
    for name, prog in {"pc_zp4": prog_generic(pc_zp4), "g64_int8": prog_generic(g64_int8)}.items():
        m = g.convert(prog)
        g.save_and_compile(m, out, f"probe_{name}")
        names.append(f"probe_{name}")
    with open(os.path.join(out, "probe.txt"), "a") as f:
        f.write("\n".join(names) + "\n")
