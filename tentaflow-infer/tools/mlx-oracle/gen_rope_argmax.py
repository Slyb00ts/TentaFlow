# =============================================================================
# Plik: gen_rope_argmax.py
# Opis: Wektory wzorcowe dla RoPE i wyboru argmax. Oba liczy MLX: RoPE przez
#       `mx.fast.rope`, argmax przez `mx.argmax` — łącznie z regułą remisu,
#       której nie da się wyczytać z dokumentacji, a która decyduje o tokenie.
# Użycie: ./mlxenv/bin/python gen_rope_argmax.py <katalog-checkpointu> <plik>
# =============================================================================

import json
import struct
import sys
from pathlib import Path

import mlx.core as mx
import numpy as np

HEADS = 32
POSITION = 137
VOCAB = 32128
SEED = 20260802


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__)
        return 2
    ckpt, out_path = Path(sys.argv[1]), Path(sys.argv[2])
    cfg = json.loads((ckpt / "config.json").read_text())
    head_dim = cfg["head_dim"]
    theta = float(cfg["rope_theta"])
    # Brak `rope_traditional` w konfiguracji znaczy wariant domyślny MLX, czyli
    # obrót par oddalonych o połowę wymiaru — nie par sąsiadujących.
    traditional = bool(cfg.get("rope_traditional", False))
    assert not traditional, "ten generator opisuje wariant half-split"

    mx.random.seed(SEED)
    q = (mx.random.normal((1, HEADS, 1, head_dim)) * 0.5).astype(mx.float16)
    rotated = mx.fast.rope(
        q, head_dim, traditional=traditional, base=theta, scale=1.0, offset=POSITION
    )
    mx.eval(rotated)

    # Prawda w f64, z tej samej definicji: theta_i = pos * base^(-2i/dims).
    q64 = np.array(q.astype(mx.float32), copy=False).astype(np.float64).reshape(HEADS, head_dim)
    half = head_dim // 2
    i = np.arange(half, dtype=np.float64)
    freq = POSITION * np.power(theta, -2.0 * i / head_dim)
    c, s = np.cos(freq), np.sin(freq)
    truth = np.empty_like(q64)
    truth[:, :half] = q64[:, :half] * c - q64[:, half:] * s
    truth[:, half:] = q64[:, :half] * s + q64[:, half:] * c

    mlx_rot = (
        np.array(rotated.astype(mx.float32), copy=False)
        .astype(np.float64)
        .reshape(HEADS, head_dim)
    )
    rel = float(np.linalg.norm(mlx_rot - truth) / np.linalg.norm(truth))
    print(f"head_dim={head_dim} theta={theta} pos={POSITION}")
    print(f"  rope: MLX wobec prawdy {rel:.3e}")

    # --- argmax ---
    mx.random.seed(SEED + 1)
    logits = mx.random.normal((VOCAB,)).astype(mx.float32)
    # Remis wstawiony celowo: dwa maksima o tej samej wartości. MLX wybiera
    # PIERWSZE wystąpienie, a kernel musi robić to samo, inaczej model
    # rozjeżdża się na jednym tokenie na kilka tysięcy i nikt nie wie dlaczego.
    top = float(mx.max(logits).item()) + 1.0
    idx_a, idx_b = 1000, 20000
    logits = mx.concatenate(
        [logits[:idx_a], mx.array([top]), logits[idx_a + 1 : idx_b], mx.array([top]), logits[idx_b + 1 :]]
    )
    argmax = int(mx.argmax(logits).item())
    mx.eval(logits)
    print(f"  argmax: {argmax} (remis na {idx_a} i {idx_b}, MLX bierze pierwszy)")
    assert argmax == idx_a, "MLX zmienił regułę remisu — sprawdź, zanim ruszysz kernel"

    blobs = [
        ("q", bytes(memoryview(q.astype(mx.float16)))),
        ("rope_mlx", bytes(memoryview(rotated.astype(mx.float16)))),
        ("rope_true", truth.astype(np.float64).tobytes()),
        ("logits", bytes(memoryview(logits))),
    ]
    with out_path.open("wb") as f:
        f.write(b"RPAM")
        f.write(struct.pack("<IIIII", 1, HEADS, head_dim, POSITION, argmax))
        f.write(struct.pack("<f", theta))
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
