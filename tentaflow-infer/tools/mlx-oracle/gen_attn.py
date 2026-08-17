# =============================================================================
# Plik: gen_attn.py
# Opis: Wektory wzorcowe dla uwagi w kroku dekodowania: jedno zapytanie wobec
#       całego cache'u KV, z grupowaniem głowic (GQA). Wyrocznią jest
#       `mx.fast.scaled_dot_product_attention`, a prawdą ten sam rachunek
#       w f64.
#
#       GQA jest tu istotą testu: 32 głowice zapytań na 8 głowic KV znaczy, że
#       cztery zapytania dzielą jeden strumień kluczy. Zła mapa nie zmienia ani
#       kształtu, ani normy wyniku — daje model, który czyta cudzą pamięć.
# Użycie: ./mlxenv/bin/python gen_attn.py <katalog-checkpointu> <plik>
# =============================================================================

import json
import math
import struct
import sys
from pathlib import Path

import mlx.core as mx
import numpy as np

SEQ = 512
SEED = 20260802


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__)
        return 2
    ckpt, out_path = Path(sys.argv[1]), Path(sys.argv[2])
    cfg = json.loads((ckpt / "config.json").read_text())
    heads = cfg["num_attention_heads"]
    kv_heads = cfg["num_key_value_heads"]
    dim = cfg["head_dim"]
    scale = 1.0 / math.sqrt(dim)

    mx.random.seed(SEED)
    q = (mx.random.normal((1, heads, 1, dim)) * 0.3).astype(mx.float16)
    k = (mx.random.normal((1, kv_heads, SEQ, dim)) * 0.3).astype(mx.float16)
    v = (mx.random.normal((1, kv_heads, SEQ, dim)) * 0.3).astype(mx.float16)

    out = mx.fast.scaled_dot_product_attention(q, k, v, scale=scale, mask=None)
    mx.eval(out, q, k, v)

    q64 = np.array(q.astype(mx.float32), copy=False).astype(np.float64).reshape(heads, dim)
    k64 = (
        np.array(k.astype(mx.float32), copy=False)
        .astype(np.float64)
        .reshape(kv_heads, SEQ, dim)
    )
    v64 = (
        np.array(v.astype(mx.float32), copy=False)
        .astype(np.float64)
        .reshape(kv_heads, SEQ, dim)
    )
    truth = np.empty((heads, dim), dtype=np.float64)
    per_kv = heads // kv_heads
    for h in range(heads):
        kv = h // per_kv
        s = (k64[kv] @ q64[h]) * scale
        s -= s.max()
        p = np.exp(s)
        truth[h] = (p @ v64[kv]) / p.sum()

    mlx64 = (
        np.array(out.astype(mx.float32), copy=False)
        .astype(np.float64)
        .reshape(heads, dim)
    )
    rel = float(np.linalg.norm(mlx64 - truth) / np.linalg.norm(truth))
    print(f"heads={heads} kv_heads={kv_heads} dim={dim} seq={SEQ} scale={scale:.6f}")
    print(f"  uwaga: MLX wobec prawdy {rel:.3e}")

    blobs = [
        ("q", bytes(memoryview(q.astype(mx.float16)))),
        ("k", bytes(memoryview(k.astype(mx.float16)))),
        ("v", bytes(memoryview(v.astype(mx.float16)))),
        ("out_mlx", bytes(memoryview(out.astype(mx.float16)))),
        ("out_true", truth.astype(np.float64).tobytes()),
    ]
    with out_path.open("wb") as f:
        f.write(b"ATT1")
        f.write(struct.pack("<IIIII", 1, heads, kv_heads, dim, SEQ))
        f.write(struct.pack("<f", scale))
        f.write(struct.pack("<I", len(blobs)))
        for name, data in blobs:
            key = name.encode()
            f.write(struct.pack("<I", len(key)))
            f.write(key)
            f.write(struct.pack("<I", len(data)))
            f.write(data)
    print(f"zapisano {out_path} ({out_path.stat().st_size} B)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
