# =============================================================================
# Plik: gen_qmm.py
# Opis: Wzorce dla BATCHOWEGO dequant-matmul — tej samej operacji co gen_qmv.py,
#       ale na wielu tokenach naraz. To jest ścieżka prefillu: wagi czytane raz
#       obsługują cały kafel tokenów.
#
#       Liczba tokenów jest CELOWO niepodzielna przez kafel kernela. Ogon jest
#       jedynym miejscem, w którym kernel musi zapytać, ile tokenów mu jeszcze
#       zostało, i wersja bez tego pytania przechodzi każdy test o równej liczbie.
# Użycie: ./mlxenv/bin/python gen_qmm.py <katalog-checkpointu> <plik-wyjściowy>
# =============================================================================

import json
import struct
import sys
from pathlib import Path

import mlx.core as mx
import numpy as np

TENSORS = [
    "model.layers.0.self_attn.q_proj",
    "model.layers.0.mlp.down_proj",
]
ROWS = 128
TOKENS = 13
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
        # Każdy token INNY. Gdyby wiersze były swoimi kopiami, kernel liczący
        # wszystkie tokeny z pierwszego wiersza przeszedłby ten test.
        x = mx.random.normal((TOKENS, cols)).astype(mx.float16)

        y32 = (x.astype(mx.float32) @ deq.astype(mx.float32).T).astype(mx.float32)
        ones = mx.ones_like(s)
        zeros = mx.zeros_like(b)
        raw_q = mx.dequantize(w, ones, zeros, group_size=group_size, bits=bits, mode="affine")
        mx.eval(y32, deq, raw_q)

        q64 = np.array(raw_q.astype(mx.float32), copy=False).astype(np.float64)
        s64 = np.array(s.astype(mx.float32), copy=False).astype(np.float64)
        b64 = np.array(b.astype(mx.float32), copy=False).astype(np.float64)
        w64 = q64 * np.repeat(s64, group_size, axis=1) + np.repeat(b64, group_size, axis=1)
        x64 = np.array(x.astype(mx.float32), copy=False).astype(np.float64)
        y64 = x64 @ w64.T

        cases.append(
            {
                "name": name,
                "rows": rows,
                "cols": cols,
                "packed": bytes(memoryview(w)),
                "scales": bytes(memoryview(s)),
                "biases": bytes(memoryview(b)),
                "x": bytes(memoryview(x)),
                "y": bytes(memoryview(y32)),
                "y64": y64.astype(np.float64).tobytes(),
            }
        )
        rel = float(
            np.linalg.norm(np.array(y32, copy=False).astype(np.float64) - y64)
            / np.linalg.norm(y64)
        )
        print(f"  {name}: [{TOKENS}, {rows}] z [{rows}, {cols}], "
              f"rel_l2(MLX f32 wobec f64) {rel:.3e}")

    with out_path.open("wb") as f:
        f.write(b"QMM1")
        f.write(struct.pack("<IIII", 1, group_size, bits, TOKENS))
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
