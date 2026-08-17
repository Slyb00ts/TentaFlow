# =============================================================================
# Plik: gen_logits.py
# Opis: Logity całego modelu dla krótkiej sekwencji tokenów, policzone przez
#       mlx-lm. To jest wyrocznia dla pętli dekodowania: sprawdza nie pojedynczy
#       kernel, tylko ich złożenie — kolejność operacji w warstwie, cache KV,
#       pozycje RoPE i wiązanie głowicy wyjściowej.
#
#       Zapisywane są logity po KAŻDYM tokenie, nie tylko po ostatnim. Rozjazd
#       narastający przez czterdzieści warstw wygląda tak samo jak błąd
#       arytmetyczny, dopóki nie widać, na którym kroku się zaczyna.
# Użycie: ./mlxenv/bin/python gen_logits.py <katalog-checkpointu> <plik>
# =============================================================================

import struct
import sys
from pathlib import Path

import mlx.core as mx
import numpy as np
from mlx_lm import load

# Krótko, bo każdy token to pełny przebieg przez model, a chodzi o poprawność,
# nie o długość. Wartości ustalone, żeby fikstura była odtwarzalna.
TOKENS = [1, 4321, 913, 27, 8]


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__)
        return 2
    ckpt, out_path = Path(sys.argv[1]), Path(sys.argv[2])

    model, _tokenizer = load(str(ckpt))
    logits_per_step = []
    for step in range(1, len(TOKENS) + 1):
        prompt = mx.array([TOKENS[:step]])
        out = model(prompt)
        mx.eval(out)
        last = np.array(out[0, -1].astype(mx.float32), copy=False).astype(np.float32)
        logits_per_step.append(last)
        top = int(np.argmax(last))
        print(f"  krok {step}: argmax {top}, max {last.max():.4f}, min {last.min():.4f}")

    vocab = logits_per_step[0].shape[0]
    with out_path.open("wb") as f:
        f.write(b"LOG1")
        f.write(struct.pack("<III", 1, len(TOKENS), vocab))
        for t in TOKENS:
            f.write(struct.pack("<I", t))
        for row in logits_per_step:
            f.write(row.astype(np.float32).tobytes())
    print(f"tokeny={TOKENS} vocab={vocab}")
    print(f"zapisano {out_path} ({out_path.stat().st_size} B)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
