# =============================================================================
# Plik: gen_embed.py
# Opis: Wektory wzorcowe dla pobrania wiersza skwantyzowanego embeddingu.
#       Tabela embeddingu jest w MLX kwantyzowana tak samo jak projekcje, więc
#       „pobranie" to w istocie dekwantyzacja JEDNEGO wiersza — i to jest
#       jedyne miejsce w kroku, gdzie czyta się po indeksie tokena, a nie po
#       kolei. Zły offset daje poprawny kształt i cudze słowo.
# Użycie: ./mlxenv/bin/python gen_embed.py <katalog-checkpointu> <plik>
# =============================================================================

import json
import struct
import sys
from pathlib import Path

import mlx.core as mx
import numpy as np

# Tokeny z różnych miejsc tabeli: początek, środek, koniec. Muszą być NIEZEROWE
# — w tym słowniku 140 z 32128 wierszy jest wyzerowanych (m.in. 63, 64, 100),
# a na zerach błędny offset też daje zera i test przechodzi nic nie sprawdzając.
# Asercja niżej pilnuje, żeby nikt nie wpisał tu takiego wiersza przez przypadek.
TOKENS = [0, 1, 131, 12345, 16133, 32127]


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__)
        return 2
    ckpt, out_path = Path(sys.argv[1]), Path(sys.argv[2])
    cfg = json.loads((ckpt / "config.json").read_text())
    q = cfg.get("quantization") or cfg["quantization_config"]
    group_size, bits = q["group_size"], q["bits"]

    weights = mx.load(str(ckpt / "model.safetensors"))
    w = weights["model.embed_tokens.weight"]
    s = weights["model.embed_tokens.scales"]
    b = weights["model.embed_tokens.biases"]
    vocab, packed_cols = w.shape
    hidden = packed_cols * (32 // bits)
    print(f"vocab={vocab} hidden={hidden} group={group_size} bits={bits}")

    rows = []
    for token in TOKENS:
        assert 0 <= token < vocab, token
        ones = mx.ones_like(s[token : token + 1])
        zeros = mx.zeros_like(b[token : token + 1])
        raw = mx.dequantize(
            w[token : token + 1], ones, zeros, group_size=group_size, bits=bits, mode="affine"
        )
        mx.eval(raw)
        q64 = np.array(raw.astype(mx.float32), copy=False).astype(np.float64).reshape(-1)
        s64 = np.array(s[token].astype(mx.float32), copy=False).astype(np.float64)
        b64 = np.array(b[token].astype(mx.float32), copy=False).astype(np.float64)
        truth = q64 * np.repeat(s64, group_size) + np.repeat(b64, group_size)
        peak = float(np.abs(truth).max())
        assert peak > 1e-4, (
            f"token {token} ma wyzerowany embedding — na zerach test nie odróżnia "
            f"poprawnego odczytu od błędnego offsetu"
        )
        rows.append((token, truth))
        print(f"  token {token:6d}: |w| max {peak:.5f}")

    # Cała tabela idzie do fikstury tylko w części potrzebnej testowi: same
    # wiersze branych tokenów, a nie 32 tysiące.
    blobs = []
    for token, truth in rows:
        blobs.append((f"packed_{token}", bytes(memoryview(w[token : token + 1]))))
        blobs.append((f"scales_{token}", bytes(memoryview(s[token : token + 1]))))
        blobs.append((f"biases_{token}", bytes(memoryview(b[token : token + 1]))))
        blobs.append((f"true_{token}", truth.astype(np.float64).tobytes()))

    with out_path.open("wb") as f:
        f.write(b"EMB1")
        f.write(struct.pack("<IIIII", 1, group_size, bits, hidden, len(TOKENS)))
        for token, _ in rows:
            f.write(struct.pack("<I", token))
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
