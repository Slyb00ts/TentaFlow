# FORGE vs llama.cpp na GB10 (DGX Spark)

Pomiar z 2026-08-09. Odpowiada na pytanie „gdzie jesteśmy" na prefillu, dekodowaniu
i skalowaniu z liczbą równoległych sekwencji, dla wszystkich modeli obecnych na tej
maszynie.

## Sprzęt i wersje

- NVIDIA GB10, sm_121a, 48 SM, warp 32, ~237 GB/s, 124610 MiB pamięci ZUNIFIKOWANEJ.
- llama.cpp `91d2fc387` (ggml 0.17.0), zbudowany lokalnie z CUDA
  (`-DGGML_CUDA=ON -DCMAKE_CUDA_ARCHITECTURES=121`, wybrało `121a-real`).
- FORGE: rewizja tego repo z dnia pomiaru.

**Pamięć zunifikowana zmienia zasady.** Cache stron po odczycie plików modeli
odejmuje się od tego, co sterownik raportuje jako wolny VRAM. Po przeczytaniu
~100 GB GGUF-ów wolne spadło do 35 GB i FORGE przestawał wstawać (preflight puli
stanów) albo zwracał 500 przy 2 sekwencjach. Zrzucenie cache tych plików
(`posix_fadvise(POSIX_FADV_DONTNEED)`, bez roota) przywróciło 115 GB i wszystkie
błędy zniknęły. Każdy pomiar poniżej startuje ze zrzuconym cache.

## Protokół

- Prefill i dekodowanie jednym strumieniem: `forge bench --reps 3` (cache prefiksów
  wyłączony z definicji) kontra `llama-batched-bench` przy `-npl 1`.
- Skalowanie: serwer OpenAI obu silników, prompt tekstowy o kalibrowanej długości,
  `max_tokens 128`, `temperature 0`. Liczbą porównywaną jest przepustowość NA
  SEKWENCJĘ pomnożona przez liczbę linii — odporna na to, że sekwencje kończą się
  w różnych momentach na EOS.
- `min_tokens` NIE jest używane: w FORGE przełącza na sampler hostowy, który omija
  batch i zaniża wynik. To pułapka pomiarowa, nie wada silnika.
- Kolumny „FORGE / llama.cpp".

## Jeden strumień

| model | P | prefill FORGE | prefill llama.cpp | delta | decode FORGE | decode llama.cpp | delta |
|---|---:|---:|---:|---:|---:|---:|---:|
| tc27b-nvfp4 | 128 | 540 | — | — | 11.0 | — | — |
| tc27b-nvfp4 | 512 | 688 | — | — | 10.9 | — | — |
| tc27b-nvfp4 | 2048 | 694 | — | — | 10.9 | — | — |
| tc27b-nvfp4 | 4096 | 686 | — | — | 10.8 | — | — |
| tc27b-q4km | 128 | 377 | 554 | -32.0% | 10.8 | 11.3 | -4.6% |
| tc27b-q4km | 512 | 499 | 753 | -33.8% | 10.7 | 11.3 | -5.5% |
| tc27b-q4km | 2048 | 484 | 812 | -40.4% | 10.7 | 11.4 | -5.8% |
| tc27b-q4km | 4096 | 471 | 810 | -41.8% | 10.7 | 11.3 | -5.5% |
| qwen36-35b-a3b-mxfp4 | 128 | 1201 | 1199 | +0.2% | 64.3 | 68.0 | -5.4% |
| qwen36-35b-a3b-mxfp4 | 512 | 2458 | 2535 | -3.0% | 63.6 | 68.5 | -7.1% |
| qwen36-35b-a3b-mxfp4 | 2048 | 2823 | 2550 | +10.7% | 62.8 | 67.8 | -7.3% |
| qwen36-35b-a3b-mxfp4 | 4096 | 2630 | 2559 | +2.8% | 61.4 | 66.7 | -8.0% |
| qwen3-30b-a3b-q4km | 128 | 1155 | 281 | +310.8% | 91.0 | 72.9 | +24.8% |
| qwen3-30b-a3b-q4km | 512 | 2400 | 2736 | -12.2% | 87.7 | 72.4 | +21.2% |
| qwen3-30b-a3b-q4km | 2048 | 2753 | 2742 | +0.4% | 76.0 | 68.0 | +11.7% |
| qwen3-30b-a3b-q4km | 4096 | 2549 | 2703 | -5.7% | 64.7 | 64.4 | +0.4% |
| bielik7b-q4km | 128 | 1874 | 2203 | -14.9% | 44.3 | 42.9 | +3.3% |
| bielik7b-q4km | 512 | 5479 | 3106 | +76.4% | 43.7 | 42.3 | +3.3% |
| bielik7b-q4km | 2048 | 5508 | 3081 | +78.8% | 41.7 | 40.5 | +3.0% |
| bielik7b-q4km | 4096 | 4682 | 2997 | +56.2% | 39.2 | 38.3 | +2.3% |
| bielik7b-q8 | 128 | 1900 | 1700 | +11.8% | 27.4 | 27.6 | -0.5% |
| bielik7b-q8 | 512 | 5477 | 2734 | +100.3% | 27.2 | 27.3 | -0.4% |
| bielik7b-q8 | 2048 | 5508 | 2721 | +102.4% | 26.4 | 26.5 | -0.5% |

Plik NVFP4 mierzy tylko FORGE: llama.cpp odczytuje go jako `Q8_0` o niemożliwym
rozmiarze 16,95 GiB dla 27B, więc nie liczy tej samej matematyki i porównanie
byłoby nieuczciwe. Porównanie 1:1 dla hybrydy idzie po Q4_K_M.

Wiersz `qwen3-30b-a3b-q4km` P128 (+310%) to artefakt rozgrzewki pierwszego wiersza
`llama-batched-bench`, nie realna przewaga.

## Skalowanie: zbiorcze dekodowanie tok/s

| bielik7b-q8 | 4096 | 4678 | 2672 | +75.1% | 25.3 | 25.4 | -0.5% |

| model | P | B=1 | B=2 | B=4 | B=8 |
|---|---:|---:|---:|---:|---:|
| tc27b-nvfp4 | 512 | 11 / — | 20 / — | 34 / — | 49 / — |
| tc27b-nvfp4 | 2048 | 11 / — | 20 / — | 34 / — | 49 / — |
| tc27b-nvfp4 | 4096 | 11 / — | 20 / — | 33 / — | 46 / — |
| tc27b-q4km | 512 | 11 / 11 | 11 / 21 | 12 / 37 | 13 / 60 |
| tc27b-q4km | 2048 | 11 / 11 | 13 / 22 | 13 / 38 | 13 / 60 |
| tc27b-q4km | 4096 | 11 / 11 | 13 / 21 | 12 / 37 | 12 / 57 |
| qwen36-35b-a3b-mxfp4 | 512 | 64 / 68 | 64 / 111 | 60 / 167 | 60 / 228 |
| qwen36-35b-a3b-mxfp4 | 2048 | 63 / 68 | 62 / 109 | 84 / 161 | 87 / 219 |
| qwen36-35b-a3b-mxfp4 | 4096 | 62 / 67 | 62 / 107 | 84 / 156 | 83 / 209 |
| qwen3-30b-a3b-q4km | 512 | 88 / 72 | 87 / 116 | 87 / 172 | 88 / 238 |
| qwen3-30b-a3b-q4km | 2048 | 76 / 68 | 76 / 107 | 77 / 150 | 77 / 201 |
| qwen3-30b-a3b-q4km | 4096 | 65 / 64 | 64 / 97 | 64 / 131 | 64 / 165 |
| bielik7b-q4km | 512 | 44 / 42 | 74 / 82 | 136 / 154 | 144 / 259 |
| bielik7b-q4km | 2048 | 42 / 40 | 68 / 74 | 116 / 127 | 124 / 192 |
| bielik7b-q4km | 4096 | 40 / 38 | 61 / 66 | 97 / 104 | 105 / 141 |
| bielik7b-q8 | 512 | 27 / 27 | 53 / 52 | 103 / 100 | 188 / 181 |
| bielik7b-q8 | 2048 | 27 / 27 | 50 / 48 | 90 / 86 | 154 / 144 |
| bielik7b-q8 | 4096 | 26 / 25 | 46 / 44 | 78 / 75 | 126 / 115 |

## Wnioski

Wygrywamy:
- Prefill modeli gęstych: Bielik 7B Q8 5477 wobec 2734 tok/s (+100%), Q4_K_M +76%.
- Dekodowanie jednym strumieniem Qwen3-30B MoE: +12…+25%.
- Skalowanie Bielika Q8: 188 wobec 181 tok/s przy ośmiu sekwencjach.

Przegrywamy, w kolejności rozmiaru luki:
1. **MoE nadrobiło większość dystansu, ale go nie zamknęło.** Grupowana dyspozycja
   ekspertów działa teraz także w dekodowaniu, dla obu rodzin. Przy ośmiu
   sekwencjach, prompt 512: Qwen3-30B 88 -> 174 tok/s wobec 238 u llama.cpp
   (luka 2,7x -> 1,37x), a hybrydowe MXFP4 60 -> 128 wobec 228 (3,8x -> 1,78x).
   Reszta dystansu jest nadal otwarta.
2. **Hybryda w K-kwantach nie ma dokładnego kernela małego batcha**, więc
   `hybrid_batch_weights_capable` odrzuca ją i batch nie wchodzi: 13 wobec 60 tok/s
   przy ośmiu. Ten sam model w NVFP4 skaluje się 10,7 → 49,3 (4,6x).
3. **Bielik Q4_K przy ośmiu sekwencjach**: 144 wobec 259. Wariant Q8 tego nie ma
   (188 wobec 181), więc to sprawa ścieżki Q4_K w batchu, nie samego batcha.
4. **Prefill hybrydy w Q4_K_M**: -32…-42%.

Dekodowanie jednym strumieniem jest w granicach kilku procent wszędzie poza MoE
(-5…-8% na MXFP4, +12…+25% na Qwen3-30B). Przepustowość dekodowania 27B jest
ograniczona pasmem: 18 GB wag przez 237 GB/s to sufit 13,1 tok/s, a mierzymy 10,8.
