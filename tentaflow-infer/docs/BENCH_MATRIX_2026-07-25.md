# Macierz porównawcza FORGE vs vLLM vs llama.cpp — 2026-07-25

GPU: RTX 4090, wolne (żadnej innej instancji na karcie). Silniki uruchamiane
pojedynczo. FORGE: commit `810592b9`. vLLM: `vllm/vllm-openai:latest` (0.25.1).
llama.cpp: build CUDA `76f46ad2`, `-ngl 99 -fa on`.

## Metodyka i jej granice

Dwa niezależne stanowiska, bo jedno nie wystarcza:

1. **Uprząż HTTP** — `vllm bench serve --backend openai`, ten sam klient dla
   każdego silnika. Nadaje się WYŁĄCZNIE gdy tokenizer klienta jest tym samym
   tokenizerem, którego używa serwer. Klient tokenizuje prompty i **liczy też
   wyjście** własnym tokenizerem, więc przy niezgodności zniekształca oba końce:
   losowe prompty „1024-tokenowe" z tokenizera Bielika re-tokenizują się do
   ~2900-3000 tokenów pod Mistralem, co dało 223 odrzucone żądania
   (`request (3028 tokens) exceeds the available context size`) i zaniżone
   liczniki wyjścia. Dlatego tą uprzężą mierzymy tylko Bielika (FORGE i vLLM
   ładują dokładnie ten snapshot, którego tokenizer dostaje klient).
2. **Natywne benche z dokładną liczbą tokenów** — `forge bench` i `llama-bench`
   generują prompt o dokładnie 1024 tokenach z własnego tokenizera modelu, więc
   niezgodność przestaje istnieć. Cena: tylko jeden strumień.

Skutek: współbieżność mamy rzetelnie dla pary FORGE/vLLM na Bieliku, a
prefill/decode dla wszystkich trzech silników na pojedynczym strumieniu.
Rzetelna współbieżność z llama.cpp wymaga albo tokenizerów HF do plików GGUF,
albo batchowego bencha w FORGE — nie ma dziś ani jednego, ani drugiego.

## 1. Bielik-PL-Minitron-7B NVFP4 — współbieżność, FORGE vs vLLM

llama.cpp nie ładuje tego checkpointu (compressed-tensors NVFP4).

### decode-only (in 32 / out 256)

| C | FORGE tok/s | vLLM tok/s | FORGE/vLLM | TPOT FORGE | TPOT vLLM |
|--:|---:|---:|---:|---:|---:|
| 1 | 158,5 | 165,0 | 0,96x | 6,07 ms | 6,04 ms |
| 4 | 560,6 | 653,4 | 0,86x | 6,80 | 6,06 |
| 8 | 1 010,4 | 1 271,1 | 0,79x | 7,24 | 6,18 |
| 16 | 1 687,2 | 2 364,4 | 0,71x | 8,73 | 6,58 |
| 32 | 2 746,0 | 4 113,2 | 0,67x | 10,93 | 7,52 |

Wniosek: parytet przy C=1, a potem rozjazd rosnący z batchem. Widać go wprost w
TPOT — vLLM praktycznie nie płaci za batch (6,04 → 7,52 ms od C=1 do C=32),
FORGE płaci (6,07 → 10,93 ms). To jest dziś główna luka silnika.

### p1024 / o128 (prefill przeplatany z decode)

| C | FORGE tok/s | vLLM tok/s | FORGE/vLLM | TTFT med FORGE | TTFT med vLLM |
|--:|---:|---:|---:|---:|---:|
| 1 | 141,2 | 145,1 | 0,97x | 123 ms | 102 ms |
| 4 | 393,2 | 453,1 | 0,87x | 130 ms | 202 ms |
| 8 | 631,5 | 761,0 | 0,83x | 132 ms | 77 ms |
| 16 | 747,0 | 1 032,6 | 0,72x | 206 ms | 95 ms |
| 32 | 771,0 | 853,4 | 0,90x | 205 ms | **630 ms** |

Tu obraz jest odwrotny na opóźnieniu: mediana TTFT FORGE trzyma się 123-206 ms
w całym zakresie, a vLLM skacze od 77 do 630 ms i przy C=32 jest **3,1x
gorsza**. vLLM traci też przepustowość między C=16 i C=32 (1 033 → 853 tok/s),
czego FORGE nie robi.

## 2. Pojedynczy strumień, dokładnie 1024 tokeny promptu i 128 decode

| model | FORGE prefill | llama.cpp prefill | FORGE decode | llama.cpp decode |
|---|---:|---:|---:|---:|
| qwen3-0.6B Q8_0 | 58 504 tok/s | 61 374 tok/s | 672,2 tok/s | 653,8 tok/s |
| Mistral-7B Q4_K_M | **14 772** tok/s | 12 704 tok/s | 171,1 tok/s | 182,4 tok/s |
| ThinkingCap-Qwen3.6-27B NVFP4 MTP | 2 285 tok/s | 2 752 tok/s | **111,7** tok/s | 47,9 tok/s |
| Bielik-7B NVFP4 | 13 367 tok/s | brak wsparcia | 158,4 tok/s | brak wsparcia |

Stosunki FORGE/llama.cpp: qwen0.6B prefill 0,95x i decode 1,03x; Mistral prefill
**1,16x** i decode 0,94x; 27B prefill 0,83x i decode **2,33x**.

Decode 27B to największa przewaga FORGE w całym zestawieniu i pochodzi z
natywnego MTP: 42,2 tok/s przy `--speculative off` wobec 111,7 tok/s przy
`--speculative mtp` (2,85 zaakceptowanego tokena na krok, 3,85x tokenów na
forward). llama-bench nie ma trybu spekulatywnego, więc 47,9 tok/s to liczba
llama.cpp bez spekulacji — dla uczciwości podano też nasze 42,2 tok/s bez niej,
czyli 0,88x. Przewaga jest funkcją MTP, nie samych kerneli.

## 3. Radeon RX 6900 XT (gfx1030, RDNA2) — FORGE vs llama.cpp

Osobne stanowisko: karta bez jednostki macierzowej. vLLM na niej nie startuje
(lokalny obraz to build CUDA, a `rocm/vllm` celuje w CDNA), więc jedynym
punktem odniesienia jest llama.cpp na ROCm (build `112c7815`, `-ngl 99`,
`HIP_VISIBLE_DEVICES=0`). Wszystkie pomiary p1024/tg128, ten sam kształt w obu
silnikach.

| model | FORGE prefill | llama.cpp prefill | FORGE decode | llama.cpp decode |
|---|---:|---:|---:|---:|
| qwen3-0.6B Q8_0 | **14 900** tok/s | 7 827 | **277,5** tok/s | 239,8 |
| Mistral-7B Q4_K_M | **1 734** tok/s | 1 301 | 67,0 tok/s | **79,0** |

Prefill: **1,90x** i **1,33x**. Decode: 1,16x i 0,85x. Przewaga w prefillu rośnie
z długością promptu (qwen: 1,35x przy p512, 1,90x przy p1024, 1,96x przy p2048)
— llama.cpp spada tam z 11 090 na 5 688 tok/s, a FORGE z 14 930 na 11 144.

UWAGA: `llama-bench tg128` dekoduje z PUSTYM kontekstem, a FORGE po prompcie,
więc porównanie decode jest przechylone na korzyść llama.cpp. Mimo to na Q4_K
llama.cpp wygrywa decode i to jest realna luka w naszych kernelach gemv, a nie
artefakt metody.

## 4. Znaleziska metodyczne

- **`forge bench` nie odzwierciedla domyślnych ustawień `serve`.** Auto-fp8
  prefill dla gęstego GGUF włącza się tylko w `serve`, więc `bench` zaniża
  prefill Mistrala 2,3x (6 332 wobec 14 772 tok/s z `FORGE_GEMM=fp8mod`).
  Każdy pomiar prefillu GGUF bez tej zmiennej jest nieporównywalny z produkcją.
- **Cache prefiksów łamie determinizm greedy.** `forge bench` na Mistralu Q4_K
  przerywa własną kontrolą: `greedy token IDs differ between benchmark
  repetitions`. Z `--prefix-cache off` powtórzenia są identyczne. Powtórzenie
  tego samego żądania może więc dać inne tokeny w zależności od stanu cache'u —
  do rozstrzygnięcia, czy to akceptowalna konsekwencja reuse'u KV, czy defekt.
- **vLLM regresuje na długich promptach przy C=32** (853 tok/s wobec 1 033 przy
  C=16, TTFT 630 ms) przy `--max-num-seqs 32`. Nie szukaliśmy dla niego
  lepszych nastaw, więc jego liczby przy C=32 należy czytać jako „domyślne", a
  nie „najlepsze osiągalne".
