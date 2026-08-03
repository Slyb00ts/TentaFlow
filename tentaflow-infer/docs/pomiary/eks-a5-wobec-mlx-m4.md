# EKS-A5 — nasz silnik wobec MLX na tej samej maszynie (Apple M4)

Do tej pory porównywaliśmy się z **liczbą z planu** (§7.6: 175 tok/s prefillu,
19 tok/s dekodowania), a nie z działającym programem. Ten pomiar to naprawia:
ten sam model, ten sam prompt, ta sama maszyna, w tej samej godzinie.

Model: Bielik-Minitron-7B-v3.0-Instruct-MLX-4bit. Prompt: 256 tokenów.
Wyjście: 32 tokeny. Mediana z trzech przebiegów po rozgrzewce.

MLX: `tools/mlx-oracle/bench_mlx.py`, mlx-lm krok po kroku (nie `generate`),
żeby prefill i dekodowanie dało się rozdzielić — to dwa różne ograniczenia i
jedna liczba na oba nie mówi nic.

Nasz: `cargo test --release -p forge-model --features metal --test
generate_vs_mlx -- --ignored --nocapture`.

## Wynik

| | MLX | nasz | stosunek |
|---|---:|---:|---:|
| prefill | **219,0 tok/s** | 192,1 tok/s | **88%** |
| dekodowanie | **22,4 tok/s** | 19,3 tok/s | **86%** |

## Co z tego wynika

**Cel z planu był zaniżony.** 175 tok/s prefillu to nie jest prędkość MLX na
tej maszynie — MLX robi 219. Przekroczenie tamtego progu o 10% nie oznaczało
więc dogonienia konkurenta, tylko dogonienie nieaktualnej liczby. Zapisane
tutaj, bo dokument, który podaje cel, powinien podawać też skąd ten cel
pochodzi i kiedy przestał być prawdziwy.

**Prefill: brakuje 14%.** Jesteśmy na 74% sufitu obliczeniowego (EKS-A2:
~260 tok/s), MLX na 84%. Nasz kernel macierzowy wyciąga 2,97 z 3,94 TFLOPS.

**Dekodowanie: brakuje 16%, i to jest większa luka niż wygląda.** Dekodowanie
jest ograniczone pamięcią: jeden token to jedno przejście przez 4,2 GB wag, co
przy zmierzonych 102,4 GB/s (EKS-A1) daje sufit 24,4 tok/s. MLX osiąga 22,4,
czyli **92% przepustowości**. My osiągamy 19,3, czyli 79%. Nie jest to więc
kwestia liczenia — nasz odczyt wag jest po prostu rzadszy niż jego.

Dla użytkownika czatu to ta druga liczba jest odczuwalna: prefill płaci się raz
na wiadomość, a dekodowanie na każdy token odpowiedzi.
