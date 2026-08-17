# =============================================================================
# Plik: bench_mlx.py
# Opis: Pomiar MLX na TYM SAMYM modelu, promptcie i maszynie, co nasz silnik.
#       Bez tego „szybciej niż MLX" jest porównaniem z liczbą z dokumentu, a
#       nie z działającym programem.
#
#       Mierzone osobno: przetworzenie promptu (prefill) i generowanie (decode),
#       bo to dwa różne ograniczenia — pierwsze obliczeniowe, drugie pamięciowe —
#       i jedna liczba na oba nie mówi nic.
# Użycie: ./.venv/bin/python bench_mlx.py <katalog-checkpointu> [liczba-tokenów]
# =============================================================================

import sys
import time
from pathlib import Path

import mlx.core as mx
from mlx_lm import load
from mlx_lm.models.cache import make_prompt_cache

PROMPT_TOKENS = 256
MAX_NEW = 32
REPEATS = 3


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__)
        return 2
    ckpt = Path(sys.argv[1])
    n_prompt = int(sys.argv[2]) if len(sys.argv) > 2 else PROMPT_TOKENS

    model, tokenizer = load(str(ckpt))

    # Ten sam prompt co po naszej stronie: „Stolica Polski to" z BOS, powtórzone
    # do żądanej długości. Treść nie wpływa na czas, ale ma być ta sama.
    base = tokenizer.encode("Stolica Polski to")
    bos = tokenizer.bos_token_id
    if bos is not None and (not base or base[0] != bos):
        base = [bos] + list(base)
    prompt = (base * (n_prompt // len(base) + 1))[:n_prompt]
    print(f"prompt {len(prompt)} tokenów, model {ckpt.name}")

    print(f"(kafel MLX: pelny prompt w jednym wywolaniu)")
    prefill_times, decode_times = [], []
    for r in range(REPEATS + 1):
        cache = make_prompt_cache(model)

        t0 = time.perf_counter()
        logits = model(mx.array([prompt]), cache=cache)
        mx.eval(logits)
        token = int(mx.argmax(logits[0, -1]).item())
        t1 = time.perf_counter()

        for _ in range(MAX_NEW - 1):
            logits = model(mx.array([[token]]), cache=cache)
            mx.eval(logits)
            token = int(mx.argmax(logits[0, -1]).item())
        t2 = time.perf_counter()

        # Pierwszy przebieg to rozgrzewka: kompilacja kerneli i rezydencja wag.
        if r == 0:
            print(f"  rozgrzewka: prefill {t1 - t0:.3f} s, decode {t2 - t1:.3f} s")
            continue
        prefill_times.append(t1 - t0)
        decode_times.append(t2 - t1)

    prefill = sorted(prefill_times)[len(prefill_times) // 2]
    decode = sorted(decode_times)[len(decode_times) // 2]
    print(f"MLX prefill:  {prefill:.3f} s  ({len(prompt) / prefill:.1f} tok/s)")
    print(f"MLX decode:   {decode:.3f} s  ({(MAX_NEW - 1) / decode:.1f} tok/s)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
