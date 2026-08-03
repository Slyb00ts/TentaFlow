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
generate_vs_mlx -- --ignored --nocapture --test-threads=1`.

**`--test-threads=1` nie jest ozdobnikiem.** Bez niego oba pomiary biegną
równolegle i biją się o to samo GPU: prefill spada ze 194 na 168 tok/s, a
dekodowanie z 21,9 na 12,1. Liczby wychodzą wtedy powtarzalnie złe, więc nic
nie sygnalizuje pomyłki.

## Wynik

| | MLX | nasz | stosunek |
|---|---:|---:|---:|
| prefill | **219,0 tok/s** | 200,8 tok/s | **92%** |
| dekodowanie | **22,4 tok/s** | 21,9 tok/s | **98%** |

Prefill podniósł się ze 194,1 po wprowadzeniu podwójnego buforowania w kernelu
macierzowym (EKS-A4).

### Korekta pierwszej wersji tego pomiaru

Pierwsze liczby po naszej stronie brzmiały 192,1 i **19,3** tok/s, z czego
wyszedł wniosek, że dekodowanie odstaje o 14% i że nasz odczyt wag jest
rzadszy niż MLX (79% wobec 92% sufitu pamięci). **To był błąd pomiaru, nie
kernela.** Nasza liczba pochodziła z pojedynczego ZIMNEGO przebiegu wewnątrz
większego testu, a liczba MLX z mediany po rozgrzewce. Po zrównaniu metody —
rozgrzewka, mediana z trzech, testy uruchamiane pojedynczo — wychodzi 21,9
tok/s i 92,2 GB/s, czyli **90% sufitu pamięciowego i 98% MLX**.

Kosztowało to jedną wycofaną zmianę: szerokie odczyty wag (`uint4` zamiast
`uint`) w kernelu wektorowym, napisane po to, żeby zamknąć lukę, której nie
było. Zmierzone rzetelnie dają 21,9 wobec 21,7 tok/s, czyli szum, a wymagały
przestawienia kolejności chodzenia po słowach także w formie kaflowej, żeby
utrzymać zgodność bitową obu. Wycofane.

Wniosek na przyszłość: **zimny przebieg to nie jest pomiar tej samej rzeczy, co
rozgrzana mediana.** Różnica wyszła tu na 13% i wystarczyła, żeby wskazać złe
miejsce do optymalizacji.

## Co z tego wynika

**Cel z planu był zaniżony.** 175 tok/s prefillu to nie jest prędkość MLX na
tej maszynie — MLX robi 219. Przekroczenie tamtego progu o 10% nie oznaczało
więc dogonienia konkurenta, tylko dogonienie nieaktualnej liczby. Zapisane
tutaj, bo dokument, który podaje cel, powinien podawać też skąd ten cel
pochodzi i kiedy przestał być prawdziwy.

**Prefill: brakuje 8%, i to jest jedyna realna luka.** Jesteśmy na 77% sufitu
obliczeniowego (EKS-A2: ~260 tok/s), MLX na 84%. Nasz kernel macierzowy wyciąga
3,08 z 3,94 TFLOPS.

**Dekodowanie: praktycznie na równi.** Dekodowanie jest ograniczone pamięcią:
jeden token to jedno przejście przez 4,2 GB wag, co przy zmierzonych 102,4 GB/s
(EKS-A1) daje sufit 24,4 tok/s. MLX osiąga 22,4 (92% sufitu), my 21,9 (90%).
Zostaje 2%, i to na ścianie, do której obaj już prawie dobiliśmy — tu nie ma
czego szukać.

Dla użytkownika czatu to właśnie ta liczba jest odczuwalna: prefill płaci się
raz na wiadomość, a dekodowanie na każdy token odpowiedzi. Realna luka wobec
MLX jest więc mniejsza, niż sugeruje sam prefill.
