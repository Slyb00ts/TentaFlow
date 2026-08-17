# =============================================================================
# Plik: gen_ffn.py
# Opis: Wektory wzorcowe dla całego bloku FFN jednej realnej warstwy: norma
#       RMS, projekcje gate/up, bramka SiLU i projekcja down. Zapisuje wyniki
#       POŚREDNIE, żeby rozjazd dało się zlokalizować na etapie, a nie tylko
#       stwierdzić na końcu.
#
#       Każdy etap ma dwie wartości: ścieżkę MLX w typie checkpointu i prawdę
#       w f64 liczoną z liczb całkowitych. Progiem dla kernela jest odległość
#       od prawdy, nie od MLX — inaczej mierzy się stratę wyroczni.
# Użycie: ./mlxenv/bin/python gen_ffn.py <katalog-checkpointu> <plik-wyjściowy>
# =============================================================================

import json
import struct
import sys
from pathlib import Path

import mlx.core as mx
import numpy as np

LAYER = 0
SEED = 20260802


def dequant_f64(weights, name, group_size, bits):
    """Waga w f64 bez zaokrąglenia do typu skal — to jest prawda."""
    w = weights[f"{name}.weight"]
    s = weights[f"{name}.scales"]
    b = weights[f"{name}.biases"]
    ones, zeros = mx.ones_like(s), mx.zeros_like(b)
    q = mx.dequantize(w, ones, zeros, group_size=group_size, bits=bits, mode="affine")
    mx.eval(q)
    q64 = np.array(q.astype(mx.float32), copy=False).astype(np.float64)
    s64 = np.array(s.astype(mx.float32), copy=False).astype(np.float64)
    b64 = np.array(b.astype(mx.float32), copy=False).astype(np.float64)
    return q64 * np.repeat(s64, group_size, axis=1) + np.repeat(b64, group_size, axis=1)


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__)
        return 2
    ckpt, out_path = Path(sys.argv[1]), Path(sys.argv[2])

    cfg = json.loads((ckpt / "config.json").read_text())
    q = cfg.get("quantization") or cfg["quantization_config"]
    group_size, bits = q["group_size"], q["bits"]
    eps = cfg["rms_norm_eps"]
    hidden = cfg["hidden_size"]
    inter = cfg["intermediate_size"]

    weights = mx.load(str(ckpt / "model.safetensors"))
    prefix = f"model.layers.{LAYER}"
    norm_w = weights[f"{prefix}.post_attention_layernorm.weight"]

    mx.random.seed(SEED)
    x = (mx.random.normal((hidden,)) * 0.1).astype(mx.float16)

    # --- ścieżka MLX, w typach checkpointu ---
    def mlx_rms(v):
        v32 = v.astype(mx.float32)
        scale = mx.rsqrt(mx.mean(v32 * v32) + eps)
        return (v32 * scale * norm_w.astype(mx.float32)).astype(mx.float16)

    def mlx_proj(v, name):
        w = weights[f"{name}.weight"]
        s = weights[f"{name}.scales"]
        b = weights[f"{name}.biases"]
        deq = mx.dequantize(w, s, b, group_size=group_size, bits=bits, mode="affine")
        return (deq.astype(mx.float32) @ v.astype(mx.float32)).astype(mx.float32)

    h = mlx_rms(x)
    g = mlx_proj(h, f"{prefix}.mlp.gate_proj")
    u = mlx_proj(h, f"{prefix}.mlp.up_proj")
    a = (g * mx.sigmoid(g) * u).astype(mx.float16)
    y = mlx_proj(a, f"{prefix}.mlp.down_proj")
    mx.eval(h, g, u, a, y)

    # --- prawda w f64, z tym samym wejściem ---
    x64 = np.array(x.astype(mx.float32), copy=False).astype(np.float64)
    nw64 = np.array(norm_w.astype(mx.float32), copy=False).astype(np.float64)
    h64 = x64 / np.sqrt(np.mean(x64 * x64) + eps) * nw64
    # Etapy dalej liczone z wejścia f16, którym faktycznie karmimy kernel:
    # inaczej porównanie mieszałoby błąd normy z błędem projekcji.
    h_in = np.array(h.astype(mx.float32), copy=False).astype(np.float64)
    g64 = dequant_f64(weights, f"{prefix}.mlp.gate_proj", group_size, bits) @ h_in
    u64 = dequant_f64(weights, f"{prefix}.mlp.up_proj", group_size, bits) @ h_in
    a_in = np.array(a.astype(mx.float32), copy=False).astype(np.float64)
    down64 = dequant_f64(weights, f"{prefix}.mlp.down_proj", group_size, bits)
    y64 = down64 @ a_in
    a64 = g64 / (1.0 + np.exp(-g64)) * u64

    # Druga prawda: CAŁY łańcuch w f64, od tego samego wejścia x, bez żadnego
    # zaokrąglenia pośredniego. Pierwsza (powyżej) mierzy każdy etap osobno,
    # karmiąc go wynikiem MLX; ta mierzy złożenie. Bez rozdzielenia tych dwóch
    # rzeczy dokładniejszy kernel wypada GORZEJ na ostatnim etapie, bo prawda
    # dla niego była policzona z gorszego wejścia.
    gate64 = dequant_f64(weights, f"{prefix}.mlp.gate_proj", group_size, bits)
    up64 = dequant_f64(weights, f"{prefix}.mlp.up_proj", group_size, bits)
    gc = gate64 @ h64
    uc = up64 @ h64
    ac = gc / (1.0 + np.exp(-gc)) * uc
    y_chain64 = down64 @ ac

    def f16(arr):
        return bytes(memoryview(arr.astype(mx.float16)))

    def f32(arr):
        return bytes(memoryview(arr.astype(mx.float32)))

    def f64b(arr):
        return arr.astype(np.float64).tobytes()

    blobs = [
        ("x", f16(x)),
        ("norm_w", bytes(memoryview(norm_w))),
        ("h_mlx", f16(h)),
        ("h_true", f64b(h64)),
        ("g_mlx", f32(g)),
        ("g_true", f64b(g64)),
        ("u_mlx", f32(u)),
        ("u_true", f64b(u64)),
        ("a_mlx", f16(a)),
        ("a_true", f64b(a64)),
        ("y_mlx", f32(y)),
        ("y_true", f64b(y64)),
        ("y_chain_true", f64b(y_chain64)),
    ]

    with out_path.open("wb") as f:
        f.write(b"FFN1")
        f.write(struct.pack("<IIIII", 1, group_size, bits, hidden, inter))
        f.write(struct.pack("<f", eps))
        f.write(struct.pack("<I", len(blobs)))
        for name, data in blobs:
            key = name.encode()
            f.write(struct.pack("<I", len(key)))
            f.write(key)
            f.write(struct.pack("<I", len(data)))
            f.write(data)

    rel = lambda a32, a64: float(
        np.linalg.norm(np.array(a32, copy=False).astype(np.float64) - a64)
        / np.linalg.norm(a64)
    )
    print(f"hidden={hidden} inter={inter} eps={eps}")
    print(f"  norma  : MLX wobec prawdy {rel(h.astype(mx.float32), h64):.3e}")
    print(f"  gate   : MLX wobec prawdy {rel(g, g64):.3e}")
    print(f"  bramka : MLX wobec prawdy {rel(a.astype(mx.float32), a64):.3e}")
    print(f"  down   : MLX wobec prawdy {rel(y, y64):.3e}")
    print(f"  łańcuch: MLX wobec prawdy {rel(y, y_chain64):.3e}")
    print(f"zapisano {out_path} ({out_path.stat().st_size} B)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
