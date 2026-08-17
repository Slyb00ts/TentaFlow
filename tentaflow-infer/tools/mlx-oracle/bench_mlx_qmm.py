# =============================================================================
# Plik: bench_mlx_qmm.py
# Opis: Ile trwa w MLX DOKŁADNIE to samo mnożenie, które liczy nasz kernel.
#       Bez tego „MLX jest szybszy o 5%" nie mówi, czy szybszy jest jego kernel,
#       czy to my mamy narzut poza mnożeniami — a to dwa różne zadania.
# Użycie: ./.venv/bin/python bench_mlx_qmm.py
# =============================================================================

import time

import mlx.core as mx

# Kształty warstwy Bielika-7B: (nazwa, wiersze wyjścia, kolumny wejścia).
SHAPES = [
    ("q_proj / o_proj", 4096, 4096),
    ("k_proj / v_proj", 1024, 4096),
    ("gate / up", 11264, 4096),
    ("down", 4096, 11264),
]
GROUP = 64
BITS = 4
TOKENS = 256
REPS = 20


def main() -> int:
    mx.random.seed(1)
    print(f"{TOKENS} tokenów, grupa {GROUP}, {BITS} bity")
    total = 0.0
    for name, rows, cols in SHAPES:
        w = mx.random.normal((rows, cols)).astype(mx.float16)
        wq, scales, biases = mx.quantize(w, group_size=GROUP, bits=BITS)
        x = mx.random.normal((TOKENS, cols)).astype(mx.float16)
        mx.eval(wq, scales, biases, x)

        def run():
            # transpose=True, bo waga jest [wiersze, kolumny], jak u nas.
            return mx.quantized_matmul(
                x, wq, scales, biases, transpose=True, group_size=GROUP, bits=BITS
            )

        mx.eval(run())
        # `mx.eval` MUSI być w pętli. MLX liczy leniwie, więc bez tego wszystkie
        # iteracje poza ostatnią są martwym grafem, którego nikt nie wykonuje —
        # pierwsza wersja tego pomiaru pokazała 49 TFLOPS na maszynie o szczycie
        # 3,94, co jest jedynym powodem, dla którego błąd się wydał.
        t0 = time.perf_counter()
        for _ in range(REPS):
            mx.eval(run())
        dt = (time.perf_counter() - t0) / REPS

        flops = 2.0 * rows * cols * TOKENS
        print(
            f"{name:18}: [{rows:5} x {cols:5}] {dt * 1e6:8.1f} us, "
            f"{flops / dt / 1e12:5.2f} TFLOPS"
        )
        # Waga liczby w warstwie: q i o po razie, k i v po razie, gate i up po razie.
        total += dt * (2 if name != "down" else 1)
    print(f"suma na warstwę: {total * 1e6:.1f} us, na 40 warstw {total * 40 * 1e3:.1f} ms")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
