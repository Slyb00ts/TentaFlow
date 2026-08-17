# =============================================================================
# Plik: gen_generate.py
# Opis: Wzorzec pełnej generacji: prompt w tekście, tokeny wejściowe i ciąg
#       tokenów wygenerowanych zachłannie przez mlx-lm. To jest bramka dla
#       CAŁEJ ścieżki — tokenizacji, prefillu, dekodowania i wyboru tokenu —
#       a nie dla pojedynczego kroku.
#
#       Zachłannie (temperatura zero), bo tylko wtedy wynik jest funkcją modelu,
#       a nie generatora losowego, i da się porównać token po tokenie.
# Użycie: ./mlxenv/bin/python gen_generate.py <katalog-checkpointu> <plik>
# =============================================================================

import json
import struct
import sys
from pathlib import Path

import mlx.core as mx
import numpy as np
from mlx_lm import load
from mlx_lm.sample_utils import make_sampler

PROMPT = "Stolica Polski to"
MAX_TOKENS = 12


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__)
        return 2
    ckpt, out_path = Path(sys.argv[1]), Path(sys.argv[2])

    model, tokenizer = load(str(ckpt))
    prompt_ids = tokenizer.encode(PROMPT)
    # Llama oczekuje BOS na początku. `encode` go tu nie dokłada, a bez niego
    # model dostaje zdanie zaczynające się znikąd i odpowiada bełkotem —
    # sprawdzone: „www. Wyszukiwarka kantorów" wobec sensownej kontynuacji.
    bos = tokenizer.bos_token_id
    if bos is not None and (not prompt_ids or prompt_ids[0] != bos):
        prompt_ids = [bos] + list(prompt_ids)
    print(f"prompt {PROMPT!r} -> {prompt_ids}")

    # Krok po kroku zamiast `generate`, żeby mieć pewność, że porównujemy
    # dokładnie to samo: pełny przebieg na promptcie, potem po jednym tokenie.
    from mlx_lm.models.cache import make_prompt_cache

    cache = make_prompt_cache(model)
    logits = model(mx.array([prompt_ids]), cache=cache)
    mx.eval(logits)
    generated = []
    # Margines to różnica dwóch najlepszych logitów wyrażona w ich rozpiętości.
    # Bez niego test wymagałby zgodności także tam, gdzie o wyborze decyduje
    # trzecia cyfra po przecinku, a dwie poprawne implementacje różniące się
    # kolejnością sumowania mają prawo wybrać wtedy inaczej.
    margins = []
    top3 = []

    def choose(row):
        v = np.array(row.astype(mx.float32), copy=False)
        order = np.argsort(-v)
        spread = float(v.max() - v.min())
        margin = float(v[order[0]] - v[order[1]]) / spread if spread > 0 else 0.0
        return int(order[0]), margin, [int(x) for x in order[:3]]

    token, margin, three = choose(logits[0, -1])
    generated.append(token)
    margins.append(margin)
    top3.append(three)
    for _ in range(MAX_TOKENS - 1):
        logits = model(mx.array([[token]]), cache=cache)
        mx.eval(logits)
        token, margin, three = choose(logits[0, -1])
        generated.append(token)
        margins.append(margin)
        top3.append(three)

    text = tokenizer.decode(generated)
    print(f"wygenerowane {generated}")
    print(f"tekst: {text!r}")
    tight = [i for i, m in enumerate(margins) if m < 0.05]
    print(f"marginesy: {[round(m, 4) for m in margins]}")
    print(f"kroki o marginesie < 5% rozpiętości: {tight}")
    _ = make_sampler  # wskazuje, że wybór jest zachłanny z rozmysłu

    with out_path.open("wb") as f:
        f.write(b"GEN1")
        f.write(struct.pack("<III", 1, len(prompt_ids), len(generated)))
        for t in prompt_ids:
            f.write(struct.pack("<I", t))
        for t in generated:
            f.write(struct.pack("<I", t))
        for m in margins:
            f.write(struct.pack("<f", m))
        for three in top3:
            for t in three:
                f.write(struct.pack("<I", t))
        payload = json.dumps({"prompt": PROMPT, "text": text}).encode()
        f.write(struct.pack("<I", len(payload)))
        f.write(payload)
    print(f"zapisano {out_path} ({out_path.stat().st_size} B)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
