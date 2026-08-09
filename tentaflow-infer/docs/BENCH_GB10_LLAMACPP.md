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
2. **Hybryda w K-kwantach** przestała stać w miejscu: 13 -> 38 tok/s przy ośmiu
   sekwencjach wobec 60 u llama.cpp (4,6x -> 1,57x). Batch dekodowania dostał
   własny kontrakt, luźniejszy niż ten dla prefillu B2 i MTP, bo wsadowy dp4a
   Q4_K różni się od seryjnego GEMV wyłącznie zaokrągleniem (względne L2 1,2e-4,
   pilnuje tego `batch_exactness.rs`), a to samo już wcześniej dotyczyło ścieżki
   gęstej. Bitowa zgodność została tam, gdzie jej brak psuje coś poza tekstem.
3. **Bielik Q4_K przy ośmiu sekwencjach**: 144 wobec 259. Wariant Q8 tego nie ma
   (188 wobec 181), więc to sprawa ścieżki Q4_K w batchu, nie samego batcha.
4. **Prefill hybrydy w Q4_K_M**: -32…-42%.

Dekodowanie jednym strumieniem jest w granicach kilku procent wszędzie poza MoE
(-5…-8% na MXFP4, +12…+25% na Qwen3-30B). Przepustowość dekodowania 27B jest
ograniczona pasmem: 18 GB wag przez 237 GB/s to sufit 13,1 tok/s, a mierzymy 10,8.

## Gdzie leży reszta dystansu na MoE (profil nsys, 8 sekwencji)

Rozkład czasu GPU dla Qwen3-30B-A3B Q4_K_M przy ośmiu równoległych sekwencjach:

| kernel | udział | wywołań | na wywołanie |
|---|---:|---:|---:|
| `gemm_i8mma_grouped` (gate/up ekspertów) | 41,2% | 71 648 | 147,6 us |
| `gemm_q6_k_grouped` (down ekspertów) | 19,7% | 14 332 | 353,6 us |
| `gemv_q4_k_dp4a_batch` (projekcje uwagi) | 13,3% | 85 860 | 39,7 us |
| `attn_decode_split` | 11,1% | 26 260 | 108,9 us |

Grupowane GEMM ekspertów to **61% czasu**.

Wcześniejsza wersja tej sekcji twierdziła, że `i8mma_grouped` czyta wagi
kilkudziesięciu ekspertów i przy 147 us siedzi mniej więcej na nominalnym paśmie
karty. To było liczone z ZAŁOŻONEJ liczby odrębnych ekspertów i jest nieprawdą.
Zmierzona liczba kafli (log z `moe_grouped_ffn`, osiem linii, prompt 512) ma
medianę **18**, nie kilkadziesiąt: osiem sekwencji o wspólnym prefiksie routuje
się bardzo zbieżnie. Przy 18 ekspertach jedno uruchomienie czyta 15,9 MiB wag,
co przy 147,6 us daje **108 GB/s**, a wariant Q6_K — 66 GB/s.

Sufit urządzenia zmierzony osobno (`bw.cu`, odczyt 4 GiB): **237 GB/s**. Ten sam
wzorzec dostępu co w kernelu (128 B z każdego wiersza o skoku 1152 B, footprint
1 GiB) osiąga **239 GB/s**, czyli układ wag NIE ogranicza niczego. Grupowany GEMM
ma więc ponad dwukrotny zapas do pasma.

`bench_moe_grouped.mojo` mierzy ten kernel izolowanie na kształcie decode
(K=2048, N=768, 18 kafli, 64 selekcje). Wychodzi **~125 GB/s przy danych
rezydentnych w cache** — czyli kernel nie dobija do pasma nawet wtedy, gdy pamięć
jest za darmo. Ogranicza go własny potok, nie ruch danych.

Cztery strukturalne wyjaśnienia zostały sprawdzone pomiarem i ODPADAJĄ:

| zmiana | oczekiwanie | wynik |
|---|---|---|
| `BM` 64 -> 32 | połowa pracy MMA, połowa ruchu aktywacji | 0,99x (bez zmian) |
| podwójny bufor shared | zapisy stagingu pod MMA zamiast przed nim | 0,95x (gorzej) |
| `BN` 64 -> 128 | dwa niezależne ładowania na wątek | 0,90x (gorzej) |
| zajętość 16,7% -> 33,3% | więcej warpów do ukrycia opóźnień | bez zmian |

Ostatni wiersz jest najmocniejszy: `BM=32` realnie schodzi ze 136 do 102
rejestrów i z 20,48 do 15,36 KiB shared, przez co mieści DWA bloki na SM zamiast
jednego — i nie zmienia to czasu ani o procent. Zajętość, praca macierzowa,
przeplot i równoległość kafli są więc wykluczone jako przyczyna.

Co zostaje zmierzone, ale niewyjaśnione: `Warp Cycles Per Issued Instruction`
13,07 przy `Mem Pipes Busy` 19,8% i `Max Bandwidth` 50%. Kernel czeka, ale nie na
pasmo i nie na brak warpów. Rozstrzygnięcie wymaga sampling profile'u po
instrukcjach (`--section SourceCounters`), którego ncu na tej maszynie nie
domyka — replay zawiesza się na unified memory.

Dwie rzeczy pozostają policzone i pewne, niezależnie od powyższego:
jeden `device.synchronize()` na warstwę routowaną (28 477 przerw po ~39,5 us w
profilu, dokładnie jedna na `moe_topk`) to **4% ściany**, a przerwy
międzykernelowe 2-10 us to kolejne **2,9%** — obie znikają razem z hostowym
odczytem routera, bo dopiero on pozwala nagrać ten krok jako graf.

## Po przejściu na dyspozycję adresowaną na urządzeniu

llama.cpp nie grupuje selekcji. Dla `ne2 <= MMVQ_MAX_BATCH_SIZE` (8) `MUL_MAT_ID`
idzie do `mul_mat_vec_q_moe`: siatka `(wiersze / c_rows_per_block, n_expert_used)`,
blok `(32, n_tokens)`, jeden warp na token, ekspert czytany w kernelu z
`ids[slot + token * stride]`. Zero pamięci współdzielonej, zero barier, zero
redukcji między warpami, zero sortowania i zero synchronizacji z hostem —
duplikaty odczytów eksperta zostawiają cache'owi. Sortowanie z
`cudaStreamSynchronize` jest u nich ścieżką AWARYJNĄ, nie główną.

Zmierzone u nas na kształcie decode (`bench_moe_grouped.mojo`, 18 ekspertów,
64 selekcje): kafel grupowany 123 us, dyspozycja per selekcja **63 us** dla
gate/up (1,94x) i 112 us dla `down` (1,10x). Kernele `gemv_*_gidx_batch` już
istniały dla kroku jednotokenowego — selekcji brakowało tylko informacji, czyją
aktywację czyta (`share = k`, token to `sel / k`).

Qwen3-30B-A3B Q4_K_M, prompt 512, agregat decode: **174 -> 190 tok/s** przy
ośmiu liniach (llama.cpp 238; luka 1,37x -> 1,25x) i 157,6 przy czterech
(llama.cpp 172). Wyjście osiemnastu równoległych żądań pozostaje spójne.

Rozkład czasu GPU PO zmianie (ten sam protokół, 779 kroków w oknie):

| kernel | udział | na krok | na wywołanie |
|---|---:|---:|---:|
| `attn_decode_split` | 22,0% | 48 | 226,5 us |
| `gemv_silu_q4_k` gidx (gate+up) | 20,0% | 48 | 206,3 us |
| `gemv_q6_k_dp4a` (głowa logitów) | 18,2% | **7** | 1287,8 us |
| `gemm_i8mma_impl` (projekcje) | 13,5% | 168 | 39,9 us |
| `gemv_q6_k_dp4a_gidx_batch` (down Q6_K) | 11,0% | 24 | 227,0 us |
| `gemv_q4_k_dp4a_gidx_batch` (down Q4_K) | 7,9% | 24 | 163,0 us |

Grupowane GEMM-y zniknęły z profilu, co potwierdza przełączenie. Down w Q6_K i
w Q4_K mają tę samą efektywność na bajt (364 i 347 GB/s liczone po bajtach
zgłoszonych), więc sześć bitów nie jest tu winne.

OTWARTY TROP, nierozstrzygnięty. Głowa logitów to `output.weight` Q6_K
2048 x 151936, czyli 255 MiB na odczyt — dokładnie te 1287,8 us. W profilu pada
SIEDEM razy na krok, nie raz, co odpowiada liczbie linii: `logits_gemm` ma
wsadowy przemiat tylko dla szerokości 2, 4 i 8, a poza nimi pętli po liniach.
Zaokrąglenie szerokości samej głowy w górę do 8 (artefakty `b2/b4/b8` są
zbudowane dla sm_121a, warunki `cols % 256` i warp 32 spełnione) NIE zmieniło
przepustowości ani o promil (190,0 -> 190,3), więc przyczyna leży gdzie indziej
niż w samym progu szerokości i ta zmiana nie została zachowana. Dopóki nie
wskażę miejsca wywołania, które te siedem uruchomień generuje, jest to 18%
czasu GPU leżące odłogiem.
