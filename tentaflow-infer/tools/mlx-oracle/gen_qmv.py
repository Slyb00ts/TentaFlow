# =============================================================================
# Plik: gen_qmv.py
# Opis: Wektory wzorcowe dla kernela dequant-GEMV na wagach MLX affine.
#       Wyrocznią jest złożenie `mx.dequantize` (sprawdzone już bit w bit)
#       z mnożeniem w f32 — czyli definicja matematyczna operacji, a nie
#       druga implementacja kernela.
# Użycie: ./mlxenv/bin/python gen_qmv.py <katalog-checkpointu> <plik-wyjściowy>
# =============================================================================

import json
import struct
import sys
from pathlib import Path

import mlx.core as mx
import numpy as np

# Kształty z realnej warstwy Bielika: wąska projekcja uwagi i szeroka FFN.
# Dwa różne N i dwa różne K, bo to one decydują o geometrii siatki.
TENSORS = [
    "model.layers.0.self_attn.q_proj",
    "model.layers.0.mlp.down_proj",
]
ROWS = 128  # tyle wierszy wyjścia bierzemy do fikstury
SEED = 20260802


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__)
        return 2
    ckpt, out_path = Path(sys.argv[1]), Path(sys.argv[2])

    cfg = json.loads((ckpt / "config.json").read_text())
    q = cfg.get("quantization") or cfg["quantization_config"]
    group_size, bits = q["group_size"], q["bits"]
    weights = mx.load(str(ckpt / "model.safetensors"))

    cases = []
    for name in TENSORS:
        w = weights[f"{name}.weight"][:ROWS]
        s = weights[f"{name}.scales"][:ROWS]
        b = weights[f"{name}.biases"][:ROWS]
        deq = mx.dequantize(w, s, b, group_size=group_size, bits=bits, mode="affine")
        rows, cols = deq.shape

        mx.random.seed(SEED)
        x = mx.random.normal((cols,)).astype(mx.float16)

        # Dwie wyrocznie, bo jedna nie wystarcza do ustawienia progu.
        #
        # y32 — ścieżka MLX: `dequantize` zwraca typ skal, czyli tutaj bf16,
        #       więc waga jest ZAOKRĄGLONA do ośmiu bitów mantysy zanim wejdzie
        #       do mnożenia.
        # y64 — prawda: wartość `q * skala + przesunięcie` liczona w f64 wprost
        #       z liczb całkowitych, bez tego zaokrąglenia.
        #
        # Kernel liczy w f32 bez zaokrąglania do bf16, więc porównanie go z y32
        # mierzyłoby głównie stratę wyroczni. Właściwym progiem jest: kernel ma
        # być wobec prawdy NIE GORSZY niż ścieżka MLX.
        y32 = (deq.astype(mx.float32) @ x.astype(mx.float32)).astype(mx.float32)
        ones = mx.ones_like(s)
        zeros = mx.zeros_like(b)
        raw_q = mx.dequantize(w, ones, zeros, group_size=group_size, bits=bits, mode="affine")
        mx.eval(y32, deq, raw_q)

        q64 = np.array(raw_q.astype(mx.float32), copy=False).astype(np.float64)
        s64 = np.array(s.astype(mx.float32), copy=False).astype(np.float64)
        b64 = np.array(b.astype(mx.float32), copy=False).astype(np.float64)
        # Skala i przesunięcie powtarzane na całą grupę wzdłuż K.
        s_full = np.repeat(s64, group_size, axis=1)
        b_full = np.repeat(b64, group_size, axis=1)
        w64 = q64 * s_full + b_full
        y64 = w64 @ np.array(x.astype(mx.float32), copy=False).astype(np.float64)
        y = y32

        cases.append(
            {
                "name": name,
                "rows": rows,
                "cols": cols,
                "packed": bytes(memoryview(w)),
                "scales": bytes(memoryview(s)),
                "biases": bytes(memoryview(b)),
                "x": bytes(memoryview(x)),
                "y": bytes(memoryview(y)),
                "y64": y64.astype(np.float64).tobytes(),
            }
        )
        rel = float(
            np.linalg.norm(np.array(y32, copy=False).astype(np.float64) - y64)
            / np.linalg.norm(y64)
        )
        print(f"  {name}: [{rows}, {cols}], |y| max {float(mx.max(mx.abs(y32))):.4f}, "
              f"rel_l2(MLX f32 wobec f64) {rel:.3e}")

    with out_path.open("wb") as f:
        f.write(b"QMV1")
        f.write(struct.pack("<III", 2, group_size, bits))
        f.write(struct.pack("<I", len(cases)))
        for c in cases:
            name = c["name"].encode()
            f.write(struct.pack("<I", len(name)))
            f.write(name)
            f.write(struct.pack("<II", c["rows"], c["cols"]))
            for key in ("packed", "scales", "biases", "x", "y", "y64"):
                f.write(struct.pack("<I", len(c[key])))
                f.write(c[key])
    print(f"zapisano {len(cases)} przypadków do {out_path} ({out_path.stat().st_size} B)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
