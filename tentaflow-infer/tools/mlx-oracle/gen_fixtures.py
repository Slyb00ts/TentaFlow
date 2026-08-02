# =============================================================================
# Plik: gen_fixtures.py
# Opis: Generuje wektory wzorcowe dla dekodera kwantyzacji MLX `affine` wprost
#       z biblioteki MLX. To ma być WYROCZNIA, a nie druga interpretacja tego
#       samego kodu — dlatego wartości oczekiwane liczy `mx.dequantize`,
#       a nie własna implementacja wzoru.
# Użycie: ./mlxenv/bin/python gen_fixtures.py <katalog-checkpointu> <plik-wyjściowy>
#         [nazwa,nazwa,...]   — bazowe nazwy tensorów; domyślnie zestaw dla modelu
#                               gęstego w nazewnictwie HF
# =============================================================================

import json
import struct
import sys
from pathlib import Path

import mlx.core as mx

# Tensory, na których sprawdzamy dekoder: różne kształty i różne role, w tym
# skwantyzowany embedding i głowa, bo one mają inną ścieżkę niż projekcje.
TENSORS = [
    "model.layers.0.self_attn.q_proj",
    "model.layers.0.self_attn.k_proj",
    "model.layers.0.mlp.gate_proj",
    "model.layers.0.mlp.down_proj",
    "model.layers.7.self_attn.o_proj",
    "model.embed_tokens",
    "lm_head",
]

ROWS = 2  # pełne wiersze: grupy biegną wzdłuż K, więc wiersz jest niepodzielny.
          # Dwa wystarczą: wiersze są niezależne, a pokrycie daje liczba grup w wierszu.


# Typ skal nie jest własnością formatu: mlx-lm zapisuje bfloat16, a mlx-whisper
# float16. Fikstura niesie go razem z danymi, żeby test sprawdzał to, co jest
# w pliku, a nie to, czego się spodziewamy.
PARAM_DTYPES = {mx.bfloat16: 0, mx.float16: 1}


def param_bits(arr: mx.array) -> tuple[bytes, int]:
    """Surowe bity skal/przesunięć w ICH WŁASNYM typie plus jego znacznik."""
    code = PARAM_DTYPES.get(arr.dtype)
    if code is None:
        raise SystemExit(f"nieobsługiwany typ skal: {arr.dtype}")
    return bytes(memoryview(arr)), code


def main() -> int:
    if len(sys.argv) not in (3, 4):
        print(__doc__)
        return 2
    ckpt, out_path = Path(sys.argv[1]), Path(sys.argv[2])
    tensors = sys.argv[3].split(",") if len(sys.argv) == 4 else TENSORS

    cfg = json.loads((ckpt / "config.json").read_text())
    q = cfg.get("quantization") or cfg["quantization_config"]
    group_size, bits, mode = q["group_size"], q["bits"], q.get("mode", "affine")
    print(f"tryb={mode} group_size={group_size} bits={bits}")
    if mode != "affine":
        print(f"ten generator obsługuje wyłącznie tryb affine, plik deklaruje {mode}")
        return 1

    weights = mx.load(str(ckpt / "model.safetensors"))

    cases = []
    for name in tensors:
        wkey, skey, bkey = f"{name}.weight", f"{name}.scales", f"{name}.biases"
        if wkey not in weights:
            print(f"pomijam {name} — brak w pliku")
            continue
        w = weights[wkey][:ROWS]
        s = weights[skey][:ROWS]
        b = weights[bkey][:ROWS]

        # Pełny dequant — wartości oczekiwane liczy MLX.
        deq = mx.dequantize(w, s, b, group_size=group_size, bits=bits, mode=mode)
        scale_bits, param_dtype = param_bits(s)
        bias_bits, bias_dtype = param_bits(b)
        if param_dtype != bias_dtype:
            raise SystemExit(f"{name}: skale i przesunięcia mają różne typy")

        # Samo rozpakowanie liczb całkowitych: skala 1, przesunięcie 0. Izoluje
        # kolejność bitów w upakowaniu, która jest jedyną rzeczą, jakiej nie da
        # się wyczytać z konfiguracji.
        ones = mx.ones_like(s)
        zeros = mx.zeros_like(b)
        raw = mx.dequantize(w, ones, zeros, group_size=group_size, bits=bits, mode=mode)

        mx.eval(deq, raw)
        cases.append(
            {
                "name": name,
                "rows": w.shape[0],
                "packed_cols": w.shape[1],
                "cols": deq.shape[1],
                "groups": s.shape[1],
                "param_dtype": param_dtype,
                "packed": bytes(memoryview(w)),
                "scales": scale_bits,
                "biases": bias_bits,
                "expected": bytes(memoryview(deq.astype(mx.float32))),
                "raw_q": bytes(memoryview(raw.astype(mx.float32))),
            }
        )
        print(f"  {name}: {w.shape} -> {deq.shape}, {s.shape[1]} grup")

    # Format pliku: nagłówek, potem sekwencja przypadków. Prosty i pozycyjny,
    # bo czyta go jeden test i nie ma potrzeby wciągać serde do fikstur.
    with out_path.open("wb") as f:
        f.write(b"MLXF")
        f.write(struct.pack("<III", 2, group_size, bits))
        f.write(struct.pack("<I", len(cases)))
        for c in cases:
            name = c["name"].encode()
            f.write(struct.pack("<I", len(name)))
            f.write(name)
            f.write(struct.pack("<IIIII", c["rows"], c["packed_cols"], c["cols"], c["groups"],
                                c["param_dtype"]))
            for key in ("packed", "scales", "biases", "expected", "raw_q"):
                f.write(struct.pack("<I", len(c[key])))
                f.write(c[key])
    print(f"zapisano {len(cases)} przypadków do {out_path} ({out_path.stat().st_size} B)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
