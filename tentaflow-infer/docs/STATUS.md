# FORGE — Status realizacji vs SPEC

Uczciwa inwentaryzacja tego, co jest zrobione, częściowe i nietknięte, mapowana
na sekcje `docs/SPEC.md`. **Reguła utrzymania: aktualizuj ten plik gdy domykasz
lub zaczynasz element (w tym samym commicie).**

Skala: SPEC to plan na ~30-45 inż. × 14 mies. (7 streamów). Zrobiony jest
najtrudniejszy RDZEŃ jednokartowy (kernele, silnik, KV, batching, kwantyzacja)
— produkcyjnej jakości, bramkowany testami. Poniżej reszta.

Legenda: ✅ zrobione · 🟡 częściowe · ❌ nietknięte

Ostatnia aktualizacja: 2026-07-25.

- ✅ **HTTP na R9700 ma rozdzielony pomiar prefillu i decode (2026-08-11).**
  Vulkan: llama.cpp v290, GPU1, Vulkan1. Forge: GPU0, HIP, cache/spec off.
  Prefill to prompt tok/s z pierwszego eventu SSE (C=1); decode to agregat
  completion tok/s dla C=1/2/4. Wszystkie żądania miały dokładne usage.

  | model / prompt | Vulkan prefill | Forge prefill | Vulkan decode C1/C2/C4 | Forge decode C1/C2/C4 |
  |---|---:|---:|---:|---:|
  | Bielik Q4, 512 | 2659,5 | 4209,5 | 104,7 / 190,9 / 328,4 | 76,9 / 75,3 / 74,5 |
  | Bielik Q4, 2048 | 3287,2 | 3397,6 | 97,9 / 169,5 / 267,9 | 65,3 / 64,2 / 63,7 |
  | Qwen 3.6 27B Q4, 512 | 848,9 | 1314,1 | 30,6 / 54,6 / 88,2 | 27,7 / 27,7 / 27,7 |
  | Qwen 3.6 27B Q4, 2048 | 970,4 | 1474,2 | 30,7 / 55,2 / 89,8 | 27,6 / 27,6 / 27,6 |
  | Qwen 3.5 MoE Q4, 512 | 2163,0 | 3774,7 | 118,1 / 170,6 / 257,3 | 104,0 / 105,8 / 104,3 |
  | Qwen 3.5 MoE Q4, 2048 | 3334,9 | 4899,5 | 122,2 / 185,9 / 256,1 | 102,7 / 51,6 / 101,8 |
  | Muse Glimmer Q4, 512 | 749,5 | 1415,4 | 32,0 / 59,1 / 103,8 | 20,8 / 21,1 / timeout |
  | Muse Glimmer Q4, 2048 | 957,8 | 1353,8 | 32,4 / 61,3 / 108,5 | 20,4 / 20,5 / timeout |

  Wartości Vulkan pochodzą z tej samej macierzy HTTP co C=1/2/4; Forge
  decode dla modeli gęstych pochodzi z tej samej macierzy, a MoE z ponownego
  przebiegu po dobudowaniu artefaktów gfx1201 `moe_topk_f32`, `moe_combine_f16`
  i `gemv_q4_k_dp4a_f16_gidx_batch`. Prefill Forge mierzono osobnym SSE z
  `max_tokens=1`, aby TTFT nie mieszał się z długim decode. Muse C=4 Forge
  przekroczył limit testu i nie jest wpisany jako wynik.

  `forge serve --tp 2` przeszedł realny POST HTTP 200 na GPU0+GPU1 (`world=2`)
  dla Bielika. Brak P2P nie blokuje tej ścieżki, ale wynik TP2 nie był jeszcze
  częścią powyższej macierzy przepustowości.

- ✅ **Muse Glimmer: globalna uwaga i kanały odpowiedzi są rozdzielone (2026-08-11).**
  Loader nie zakłada już `V = K` na podstawie geometrii warstwy: wybiera własną
  projekcję V wyłącznie wtedy, gdy mapa wag zawiera `AttnV`. To przywraca prawidłowe
  V w globalnych warstwach Muse, a zachowuje `V = K` dla warstw Gemmy bez tensora V.
  GPU1 oracle dla `T=60`, `Q=32`, `KV=2`, `head_dim=128`, `page=32` przeszedł dla
  scalar i WMMA względem niezależnej referencji CPU. Parser Muse przekazuje
  `assistant to=self` do `reasoning_content`, pierwszy kanał użytkownika do
  `content`, a kolejne kanały użytkownika po reasoning ignoruje, aby repetycja nie
  trafiała do odpowiedzi API. Wspólny builtin Jinja Muse jest używany przez `serve`,
  `run` i API; kończy się `<|start|>assistant to=user<|message|>`, więc kanał
  odpowiedzi jest ustalony przed generacją. HTTP na GPU1, prompt 67 tokenów: przy
  `max_tokens=512` i `2048` `content` zawiera koherentną dwuzdaniową odpowiedź po
  polsku, a `reasoning_content` pozostaje osobno.

- ✅ **Sufit dekodowania na R9700 jest ZMIERZONY i nie jest tam, gdzie zakładał
  rachunek pasma (2026-07-31).** Rachunek `16,1 GB / 552 GB/s = 34,1 tok/s`
  milcząco zakładał, że każdy bajt idzie pełnym pasmem. Nie idzie: mikrobenchmark
  na ZIMNYM DRAM odtwarza czasy kerneli z modelu co do mikrosekundy i pokazuje,
  że pasmo rośnie MONOTONICZNIE z czasem trwania kernela — 14,7 MB daje 473 GB/s,
  100 MB 582, 401 MB 597, a 1,04 GB (`lm_head`) 629. Krok dekodowania to 257
  uruchomień GEMV po 39-177 us, więc siedzi na 450-570 GB/s i żadnym
  kafelkowaniem się tego nie podnosi. Do tego dochodzi jednorodny podatek
  **3,5 us przestoju na KAŻDE uruchomienie** (1028 przerw na token, wszystkie w
  paśmie 2-5 us) — grafu HIP to nie zdejmuje, bo koszt jest po stronie GPU.
  Jedyna dźwignia to MNIEJ KERNELI. Zrobione: scalony wstęp kroku DeltaNet
  (7 uruchomień na warstwę w 1), scalony wstęp warstwy uwagi (5 w 1), szerszy
  blok normy przy dekodowaniu jednego wiersza, siatka trwała dla wąskich
  GEMV-ów — razem **1033 -> 681 uruchomień** na token i przestój **3,98 -> 2,74
  ms**, przy niezmienionej zajętości kerneli (30,65 -> 30,35 ms), bo te fuzje nie
  przyspieszają liczenia, tylko zdejmują podatek od dyspozycji. Obie fuzje są
  bitowo zgodne (blok ma tyle wątków, ile redukcja miała wartości). Wynik przy
  niezmienionej sumie SHA `0bf2b86b…` we wszystkich komórkach: wcześniejszy
  wynik Q4_K_M **31,0 tok/s** nie jest już wynikiem referencyjnym. Niezależny
  oracle ujawnił błąd wspólnego rdzenia Q4_K DP4A; po jego korekcie, bez
  ekstrapolacji, Qwen3.6-27B Q4_K na HIP/R9700 osiąga przy p1024/tg1000 po
  rozgrzewce trzy wyniki **30,9 / 30,8 / 30,7 tok/s** (mediana **30,8 tok/s**).
  NVFP4 **30,0 -> 30,9**, MTP 58,8 -> 58,9 i 70,3 -> 70,4,
  dwie karty 39,7 -> 40,0. Zostało 128 uruchomień w zasięgu tej samej metody
  (`silu_mul`, `gated_rmsnorm`, `sigmoid_mul`), warte ~0,5 ms; **34 tok/s na
  jednej karcie dla tego checkpointu nie jest osiągalne** i przez tę ścianę
  przechodzi się wyłącznie spekulacją albo drugą kartą. Pomiary, pułapka Infinity Cache i złapana regresja MTP:
  `docs/BENCH_R9700_27B.md`.
- ❌ **Dwie karty: 39,7 tok/s zamiast możliwych ~50, i wiadomo dlaczego
  (2026-07-31).** Podział obejmuje 88,5% czytanych bajtów, więc karta modelu
  powinna kończyć krok w ~20 ms; mierzymy 25,2. Profil obu agentów: karta 0 ma
  **1290 uruchomień na token wobec 745 jednokartowych** i **182 kopie D2D**
  (jednokartowo: 5), a jej zajętość to 20,1 ms wobec 13,0 ms karty
  wspierającej. Nadwyżka NIE jest złym stosunkiem podziału — sweep `--tp-split`
  pokazuje, że równy podział jest najlepszy — tylko pracą NIEPODZIELONĄ
  (projekcje uwagi, `ssm_out`, cały mikser DeltaNet, normy). Wobec przebiegu
  jednokartowego TEGO SAMEGO modelu (1001 uruchomień, 30,04 ms zajętości,
  3,81 ms przestoju) karta 0 ma +289 uruchomień (~1,0 ms) i ~2,3 ms czekania na
  kartę 1. Pełny podział warstwy dałby ~15,8 ms zajętości na rangę i krok ~21 ms,
  czyli **47-48 tok/s wobec 39,7** — cała pula, jaka jest do wzięcia na dwóch
  kartach, bo podatek od liczby uruchomień jest wspólny dla obu rang. To domyka
  rachunek za `TENSOR_PARALLEL_DESIGN.md`: dokładanie kolejnych macierzy do
  dzisiejszej protezy DOKŁADA kopie zamiast je usuwać, więc następnym krokiem
  jest SPMD, a nie kolejne wpięcie.

- ❌ **llama.cpp bije nas na Radeonie w prefillu hybrydowym: 7x na 0,8B i 26x
  na 27B (2026-07-27).** Pierwsze porównanie na RX 7900 XT z llama.cpp
  zbudowanym pod ROCm/gfx1100 (`ff067f76`). Qwen3.6-27B NVFP4 MTP, model w
  całości w VRAM (20 067 z 20 464 MiB, GPU 100%):

  | | FORGE | llama.cpp | |
  |---|--:|--:|---|
  | prefill p1024 | 27,3 tok/s | **716,3** | llama.cpp **26,2x** |
  | decode, bez spekulacji | 9,5 tok/s | **33,0** | llama.cpp **3,5x** |
  | decode, MTP po OBU stronach | 47,1 tok/s | **73,4** | llama.cpp **1,56x** |

  qwen-guard 0,8B Q8_0 (ta sama architektura): prefill 2 647,6 wobec **18 480,9**
  (llama.cpp 7,0x), decode **317,3** wobec 246,4 (FORGE 1,29x — jedyna wygrana).
  PRZYCZYNA JEST JEDNA I STRUKTURALNA: prefill FORGE jest PŁASKI względem
  długości promptu (27,3 przy p128, 27,5 przy p512, 27,3 przy p1024), czyli nie
  amortyzuje odczytu wag — hybrydowy prefill layer-major stoi na `mma`/`ldmatrix`
  i jest zabramkowany na `Vendor::Nvidia`, więc na AMD zostaje chunk T=16 i 1024
  tokeny to 64 przebiegi po całych wagach. Ta sama bramka tłumaczy obie luki.
  Potwierdzenie od drugiej strony: MTP daje w FORGE 4,96x (9,5 → 47,1), a
  spekulacja mnoży przepustowość liniowo tylko przy dominującym STAŁYM narzucie
  na krok. Model GĘSTY problemu nie ma (Bielik-7B NVFP4: 1 521 tok/s prefillu).
  KIERUNEK: największą dźwignią na AMD nie są kolejne kernele WMMA, tylko zdjęcie
  bramki `Vendor::Nvidia` z hybrydowego prefillu i przeniesienie ścieżki
  layer-major na WMMA. Pomiary, metoda i zastrzeżenia:
  `docs/BENCH_7900XT_LLAMACPP.md`.
  UWAGA: to koryguje wcześniejszy przekaz „WMMA dało +58% prefillu" — to była
  poprawa wobec NASZEGO kafla dot4, a nie dojście do konkurencji.
- ✅ **Zasięg architektury w katalogu kerneli: kernel pod jedną kartę nie
  wywraca reszty (2026-07-27).** Do tej pory jedynym sposobem wyrażenia „ten
  kernel jest tylko dla NVIDII" było „nie kompiluje się", co MIESZA projekt z
  usterką i wywracało build NVIDII przy każdym kernelu AMD-only. Teraz zasięg
  stoi przy rejestracji w `build_kernels_catalog.mojo`:
  `# arch: amd:gfx11+`, `# arch: nvidia:sm_89+`, `# arch: nvidia`; brak
  komentarza znaczy PRZENOŚNY i taki kernel musi zbudować się wszędzie.
  Architektura jest wykrywana PRZED kompilacją (`ctx.arch_name()`), więc odsiew
  następuje zanim cokolwiek padnie. AMD porównuje się po POKOLENIU
  (gfx1030 i gfx1036 to RDNA2, gfx1100 to RDNA3), NVIDIA liniowo; nazwy CDNA
  (`gfx90a`, `gfx942`) są odrzucane głośno, bo mają inny schemat numeracji i nie
  ma tu na czym tego sprawdzić.
  EFEKT: **builda AMD nie trzeba już uruchamiać w trybie częściowym** — 134
  kernele, które „nie kompilowały się" na Radeonach, to w całości rodziny
  `mma`/`ldmatrix`/FP8 NVIDII (zweryfikowane po ŹRÓDLE, nie po liście błędów).
  gfx1030 i gfx1100 budują się w trybie ścisłym i `unsupported.txt` zniknął —
  backlog portu jest pusty. Liczby w zasięgu: sm_89/sm_121 **529**,
  gfx1030 **395**, gfx1100/gfx1201 **399**.
  Wybór artefaktów w runtime jest tabelaryczny (`EMBEDDED_SETS`), a dołożenie
  karty to jeden wiersz — procedura w `docs/NOWA_KARTA.md`. Dołożona przy tym
  ASYMETRIA, której wcześniej nie było: PTX NVIDII jest przenośny w górę (JIT),
  więc zestaw `sm_89` obsługuje każdą nowszą kartę, natomiast HSACO AMD jest
  związany z ISA i wymaga DOKŁADNEGO dopasowania. Warianty zawężone do
  architektury (`sm_121a`) nie wędrują w górę.
- ✅ **FORGE liczy na Blackwellu: katalog sm_121a zbudowany na DGX Spark
  (2026-07-27).** 529 z 529 kerneli w zasięgu, czyli pełny build NVIDII
  przechodzi z deklaracjami zasięgu, a cztery kernele AMD-only są z niego
  poprawnie wykluczone — to była jedyna rzecz, której nie dało się sprawdzić
  lokalnie. Trzy usterki przenośności po drodze: (1) `pixi.toml` deklarował
  wyłącznie `linux-64`, a Spark to aarch64; (2) shim ptxas miał zaszyte
  `/opt/cuda/bin/ptxas` (Arch) i nie znajdował go na `/usr/local/cuda` (DGX) —
  teraz szuka w PATH, `CUDA_HOME` i obu lokalizacjach; (3) Mojo nazywa tam
  artefakty **`sm_121a`**, a HAL zgłasza `sm_121`, więc bez obsługi wariantu z
  literą Spark ignorowałby własny zestaw i brał sm_89.
  UWAGA: **katalog sm_89 jest W TYLE** za katalogiem źródeł (474 z 529) — brakuje
  kerneli dodanych po ostatnim buildzie na Adzie (DeepSeek, rodzina GEMM na
  instrukcjach dot). Mojo kompiluje WYŁĄCZNIE dla lokalnego GPU
  (`MOJO_TARGET_ACCELERATOR` nie działa), więc wymaga to przebudowania na RTX
  4090. Dług jest jawny w `KNOWN_STALE` w teście katalogu, a nie przemilczany.
- ✅ **FORGE liczy na RX 7900 XT (gfx1100); prefill Q8_0 przeniesiony na WMMA
  (2026-07-27).** Zmierzone p1024/tg128, oba układy pod pełnymi zegarami
  (6900 XT 2540/1000 MHz, 7900 XT 2585/1249 MHz), identyczne sumy SHA
  wygenerowanych tokenów na obu kartach:

  | model | 6900 prefill | 7900 prefill | 6900 decode | 7900 decode |
  |---|--:|--:|--:|--:|
  | qwen-guard Q8_0 (hybryda) | 1 506,4 tok/s | **2 647,6** (+75,8%) | 251,3 tok/s | **317,3** (+26,3%) |
  | OLMoE-1B-7B Q4_K_M (MoE) | 280,5 tok/s | 273,1 (-2,6%) | 88,5 tok/s | **112,6** (+27,2%) |
  | Bielik-7B NVFP4 | 1 311,1 tok/s | **1 521,8** (+16,1%) | 65,1 tok/s | **114,2** (+75,4%) |

  Prefill Q8_0 rośnie 1,76x wobec RDNA2 i **+57,7% wobec tej samej karty na
  kaflu dot4** (1 679 → 2 647,6), bo RDNA3 zdegradowała instrukcje pakowanego
  iloczynu: int8 `dot4` **97 → 43 TOPS**, f16 `dot2` 48 → 31 TFLOPS, podczas gdy
  WMMA daje **98 TOPS int8** i **102 TFLOPS f16**. Bez WMMA nowsza karta była w
  prefillu WOLNIEJSZA od starszej. Q4_K i NVFP4 nadal idą kaflem dot4 — to
  następne rodziny do przeniesienia.
  TRZY USTERKI ZNALEZIONE PRZY URUCHOMIENIU (szczegóły w `kernels/mojo/MOJO_NOTES.md`):
  (1) `v_dot4_i32_i8` na gfx11 liczy BEZ ZNAKU — asembler przyjmuje mnemonik
  RDNA2 i wykonuje go jako `iu8` z wyzerowanym `neg_lo`; `(-1,-2,-3,-4)·(4,3,2,1)`
  dawało 2540 zamiast -20, testy `gemm_q4_0`/`gemm_q6_k` miały błąd względny 635,
  a te same prompty dawały RÓŻNE tokeny na obu kartach. Poprawna forma
  (`neg_lo:[1,1]`) KOSZTUJE: 55 → 43 TOPS w izolacji i -7% prefillu Bielika,
  więc wcześniejsze 1 642 tok/s było wynikiem kernela liczącego śmieci.
  (2) `bench_dot4_i8` mierzył tylko przepustowość na dodatnich danych, więc
  przechodził mimo złej instrukcji — dopisane przypadki ze znakiem.
  (3) Dobór chunka hybrydowego prefillu zwracał 32 dla modeli bez NVFP4 nie
  pytając backendu, a T>=32 wchodzi na kafle `i8mma`, których poza NVIDIĄ nie ma
  — `qwen-guard` wywracał się na OBU Radeonach.

- ✅ **Pełny build katalogu przechodzi i cały workspace jest zielony poza jedną
  znaną rozbieżnością (2026-07-25).** Build katalogu: 80 jednostek, 6
  podzielonych automatycznie po błędzie offload kompilatora, **zapisano 474
  kernele** — i odtworzył PTX oraz manifest BAJT W BAJT względem repo, czyli
  artefakty w drzewie są dokładnie tym, co produkuje katalog. Wcześniejsza
  „awaria" builda była moją pomyłką w monitorowaniu: `pgrep -f build_kernels.mojo`
  dopasowywał własną linię poleceń bash, a proces zginął razem z pętlą
  monitorującą po timeoucie; same błędy offloadu brały się z kontencji o GPU
  (na karcie stał wtedy 27B).
  Naprawione przy tym cztery rzeczy, wszystkie moje z wcześniejszych sesji:
  (1) siedem plików testowych `forge-server` nie kompilowało się od dodania pola
  `ModelConfig::nvfp4_ct_layout` — uzupełnione o `NvFp4CtLayoutPolicy::Auto`;
  (2) `gemm_nvfp4_gguf_out_f32_{b4,b8,b16}` i nowy `gemm_nvfp4_gguf_f16_b16_nvidia`
  były zadeklarowane jako przenośne w `PORTABLE_RAW_NVFP4`, ale katalog
  kompilował je jako sm_89-only — na sm_80/sm_86 batchowa głowa logitów NVFP4 by
  się nie wczytała; dopisane do predykatu przenośności i przebudowane do
  `.target sm_80` z walidacją `ptxas -arch=sm_80`;
  (3) wyczerpanie puli VRAM zwracało `ForgeError::Device(String)` zamiast
  typowanego `OutOfMemory` — `map_err` w `CudaDevice::alloc` spłaszczał każdy
  błąd areny, więc admission i tiering widziały zwykły błąd urządzenia; nowy
  `pool_alloc_error` zachowuje wariant i loguje nazwę puli;
  (4) sześć testów `forge-kernels` dzieli grow-only scratch `prepare_q8_1` i
  nadpisywało sobie skwantyzowane aktywacje przy równoległym przebiegu (40/42
  równolegle, 42/42 przy `--test-threads=1`) — serializuje je teraz jawny mutex
  `PREPARED_Q8_SCRATCH`.
  Stan końcowy: **605 testów przechodzi, 1 nie** — `batched_matches_single_seq`,
  czyli udokumentowana wyżej rozbieżność precyzji ścieżek decode.
- ✅ **Katalog kerneli uzgodniony z manifestem (2026-07-25).** Pełny build
  katalogu USUNĄŁBY 14 żywych kerneli i przywrócił zrewertowane — bo kolejne
  sesje dodawały kernele wyłącznie izolowanymi builderami, a `gemm_q6k_i8_native_*`
  zostały w katalogu po rewercie. Usunięte jako martwe (żaden Rust ich nie
  używa, PTX nie istnieje): 12 instancji `gemm_q6k_i8_native_*` i stary sampler
  `topk_batched_f32`. Dorejestrowane: `topk_batched_{partial,final}_f32`,
  `gemv_q{4,6}_k_dp4a_batch_b{2,4,8,16}`, `pack_q{4_k,6_k,8_0}_fp8`,
  `gemm_q8_0_i8mma_b16` i `gemm_nvfp4_gguf_f16_b16_nvidia`.
  `test_catalog_matches_committed_manifest` przechodzi, czyli katalog i manifest
  opisują ten sam zestaw 474 kerneli. Nowe kernele publikuje się izolowanym
  builderem w wariancie z manifestem I rejestruje w katalogu — jedno bez
  drugiego odtworzy dryf.
- ✅ **Bramka dense prefill przestała wymagać usuniętego kernela (2026-07-25).**
  Test `dense_prefill_wymaga_artefaktow_konkretnego_hd_formatu_i_batcha`
  sprawdzał, że brak `topk_batched_f32` wyłącza dense prefill — a ten kernel
  zniknął przy wymianie samplera na dwuprzebiegowy. Produkcyjne
  `dense_prefill_artifacts_capable` wymagało już poprawnych
  `topk_batched_{partial,final}_f32`; nieaktualny był wyłącznie test.
  `forge-kernels` przechodzi w całości (42+9+70+1).
- ❌ **Strojenie głowy F16 przez większy kafel: ZMIERZONE I ODRZUCONE
  (2026-07-25).** Głowa liczy 262 MB w 563 us (465 GB/s), gdy attention w tym
  samym kroku wyciąga 776 GB/s, więc dla 32 tokenów wygląda na ograniczoną
  równoległością pamięci: kafel `gemm_f16_out_f32_bm32` ma tylko 64 wątki na
  blok wobec 256 w kaflu BM128. Hipoteza: skoro głowa jest ograniczona odczytem
  wag, dopełnienie 32 tokenów do kafla BM128 jest darmowe, a wątków jest 4x
  więcej. Zmierzone odwrotnie: decode-only C=32 dało **2 550/2 556 tok/s wobec
  2 683-2 746** na kaflu BM32, czyli około 6% GORZEJ — dodatkowa praca tensor
  cores na dopełnionych tokenach przewyższa zysk z równoległości. Wycofane;
  465 GB/s zostaje niewyjaśnione, ale łatwa dźwignia jest wyczerpana.
- ✅ **Głowa logitów ujednolicona na F16 w obu ścieżkach (2026-07-25).**
  `logits_gemv` preferował paczkę `fp8_lm_head`, a ścieżka batchowa liczyła
  głowę w F16 — więc na Bieliku z auto-fp8 pojedynczy strumień miał logity
  e4m3, a batch nie, czyli **jakość wyjścia zależała od współbieżności**. To ta
  sama wada, za którą odrzuciłem głowę FP8 w batchu, tylko z drugiej strony.
  Preferencja usunięta; paczka zostaje dla prefillu, gdzie liczą się aktywacje,
  a nie wybór tokena. Koszt: single-stream decode 158,3 → **155,0 tok/s
  (-2,1%)**, prefill bez zmian (13 364 wobec 13 346 tok/s). Weryfikacja celu:
  ten sam prompt greedy przy C=1 i C=6 daje teraz IDENTYCZNE wyjście (wcześniej
  ścieżki różniły się głową). Bramki: workspace 606/606, `batched_bielik`
  z paczkami FP8 2/2.
- ✅ **Jedno źródło iloczynu int8 dla NVIDII i AMD: 312 → 340 kerneli
  (2026-07-25).** Nowy `src/arch_dot.mojo` z `dot4_i8(a, b, c)` rozgałęzia się
  w `comptime` po `_accelerator_arch()`: `llvm.nvvm.idp4a.s.s` na NVIDII,
  instrukcja `v_dot4_i32_i8` przez `inlined_assembly` na AMD. `decode_dp4a.mojo`
  ma teraz alias `comptime _dp4a = dot4_i8`, więc **31 wywołań w pięciu plikach
  nie było ruszanych**, a implementacja jest jedna.
  Poprawność: cztery przypadki brzegowe sprawdzone na karcie — (1,2,3,4)·(4,3,2,1)
  z akumulatorem 100 daje 120, wektor ujemny 92, zero 100, maksimum
  4·127·127+100 = 64 616. Wszystkie dokładnie. Wygenerowany assembler
  `gemv_q8_0_dp4a_f16` zawiera **8 instrukcji `v_dot4_i32_i8`**, czyli kompilator
  wziął jednostkę sprzętową, a nie fallback skalarny.
  Efekt na inwentarzu: **340 z 474 kerneli kompiluje się dla gfx1030** (było 312),
  czyli jedna linia helpera odblokowała 28 kerneli — całą rodzinę dp4a.
  ZOSTAJE 134, wszystkie z powodu `mma`/`ldmatrix` albo FP8:
  46 pozostałych, 24 Q4_K/Q6_K i8 multistage, 22 NVFP4-CT (kafle BM16/BM32
  i prefill), 11 Q8_0 i8mma/triplet, 11 NVFP4 GGUF mma/tile128, 11 FP8
  (niemożliwe przed RDNA4), 9 attention/flash. Lista:
  `docs/AMD_KERNEL_INVENTORY_gfx1030.txt`.
  KIERUNEK: te 134 to nie 134 osobne zadania — to kilka kształtów kafli MMA,
  które trzeba raz napisać na `v_dot4_i32_i8` z akumulacją w rejestrach
  (odpowiednik `mma.m16n8k32.s8` bez jednostki macierzowej), po czym rodziny
  Q8_0, Q4_K/Q6_K i NVFP4 przeniosą się tak samo jak dp4a. To jest ta sama praca,
  która decyduje o celu prefillu (~5x zapasu wobec llama.cpp).
- 🟡 **Katalog kerneli buduje artefakty AMDGCN; inwentarz gfx1030: 312 z 474
  (2026-07-25).** `build_kernel_catalog.py` rozgałęzia się teraz po TREŚCI zrzutu,
  nie po rozszerzeniu pliku: obecność `.amdgcn_target` wybiera ścieżkę AMD, która
  łata identyfikator celu (`...-unknown-gfxNNNN` → bez `unknown`), składa
  assembler do HSACO przez `clang -x assembler -target amdgcn-amd-amdhsa`
  i wpisuje `nazwa.hsaco` do manifestu z symbolem z `.globl`. Ścieżka PTX jest
  nietknięta, a testy katalogu przechodzą 4/4.
  Dodany tryb `FORGE_KERNEL_BUILD_INVENTORY=1`: pojedynczy kernel, który nie
  kompiluje się dla danej architektury, jest zapisywany i build IDZIE DALEJ,
  a publikacja jest wyłączona (zestaw byłby niepełny). Wynik dla gfx1030:
  **312 z 474 kerneli kompiluje się, 162 nie** — lista w
  `docs/AMD_KERNEL_INVENTORY_gfx1030.txt`.
  Rozkład 162 nieobsługiwanych: 49 Q4_K/Q6_K, 39 pozostałych, 22 NVFP4-CT
  (kafle BM16/BM32, prefill, fp8), 19 Q8_0 (i8mma/triplet), 13 NVFP4 GGUF
  (mma/tile128), 11 FP8, 9 attention/flash. Powody sprowadzają się do
  intrinsików NVIDIA w zrzucie LLVM: `llvm.nvvm.ldmatrix.sync.aligned` i
  `llvm.nvvm.idp4a.s.s` (odpowiednik dp4a), plus 121 przypadków ogólnego
  „failed to run the pass manager", czyli tego samego po stronie MMA.
  WNIOSEK: dwie trzecie katalogu przenosi się bez pracy, a cała reszta to
  dokładnie te rodziny, które trzeba napisać na `v_dot4_i32_i8` — i to jest
  ta sama lista, która decyduje o prefillu. `idp4a` ma bezpośredni odpowiednik
  AMD, więc kernele dp4a są najtańszą i najważniejszą pozycją do przeniesienia.
- ❌ **GigaToken sprawdzony i ODRZUCONY dla ścieżki inferencji (2026-07-25).**
  GigaToken to rustowy tokenizer BPE (nie silnik), reklamowany jako 989x
  szybszy od HuggingFace i 24,53 GB/s, z obietnicą 40% redukcji TTFT.
  Ta obietnica jest liczona wobec pythonowego HuggingFace — a nasza ścieżka to
  już natywny Rust (`forge-tokenize` nad crate'em `tokenizers`, `encode_fast`).
  Zmierzone naszym nowym testem `forge-tokenize --test throughput`:
  **6,0 MB/s i 1,49 M tok/s** jednowątkowo (217 089 tokenów z 872 448 bajtów
  w 145,8 ms). Wobec prefillu:

  | ścieżka | prefill | tokenizacja szybsza | udział w TTFT |
  |---|--:|--:|--:|
  | 4090, Bielik NVFP4 | 13 364 tok/s | 111x | **0,90%** |
  | 6900 XT, llama.cpp Mistral | 1 406 tok/s | 1 060x | **0,09%** |

  Udział jest niezależny od długości promptu (oba skalują się liniowo), więc
  nawet nieskończenie szybki tokenizer urywa najwyżej 0,9% TTFT na NVIDII i
  0,09% na AMD. Profil nsys to potwierdza: host na krytycznej ścieżce to <=0,4 ms.
  GDZIE BY SIĘ PRZYDAŁ: masowa tokenizacja korpusów (pipeline'y treningowe
  ML Studio w głównym repo) — 1 GB tekstu to u nas ~2,8 min jednowątkowo. Ale
  nawet tam przewaga jest mniejsza niż reklamowana, bo nasze 6 MB/s to jeden
  wątek, a maszyna ma 16 rdzeni. Dla FORGE: bez wartości.
- ✅ **FORGE LICZY NA RADEONIE RX 6900 XT i bije llama.cpp w prefillu
  (2026-07-25).** Pierwsza realna inferencja na tej karcie: qwen3-0.6B Q8_0
  generuje spójny polski tekst, a prefill jest szybszy od llama.cpp na ROCm w
  całym zakresie długości promptu, przy czym przewaga ROŚNIE z długością:

  | model / prompt | FORGE prefill | llama.cpp | FORGE/llama.cpp |
  |---|--:|--:|--:|
  | qwen3-0.6B Q8_0, p512 | 14 929,7 tok/s | 11 090,0 | **1,35x** |
  | qwen3-0.6B Q8_0, p1024 | 14 900,2 tok/s | 7 827,3 | **1,90x** |
  | qwen3-0.6B Q8_0, p2048 | 11 144,2 tok/s | 5 687,9 | **1,96x** |
  | Mistral-7B Q4_K_M, p1024 | 1 733,8 tok/s | 1 300,6 | **1,33x** |

  Decode (tg128): qwen 277,5 wobec 239,8 (**1,16x**), Mistral **67,0 wobec 79,0
  (0,85x — tu llama.cpp wygrywa)**. Decode jest ograniczony pasmem i idzie u nas
  kernelami gemv dp4a; na Q4_K llama.cpp ma tam lepszą ścieżkę i to jest realna
  luka do zamknięcia, a nie szum pomiarowy.

  UWAGA METODOLOGICZNA: `llama-bench tg128` dekoduje z pustym kontekstem, a FORGE
  po prompcie, więc porównanie decode jest przechylone NA KORZYŚĆ llama.cpp.
  Prefill 14 914 tok/s to 17,8 TOPS efektywnie, czyli 18% zmierzonego pułapu
  int8 karty; llama.cpp jest tam na 9,3 TOPS. Zapas do pułapu zostaje duży,
  bo model 0,6B ma małe GEMM-y i dominuje narzut warstw poza GEMM.
- 📊 **Kafle int8 sa w praktycznym optimum — trzynascie prob, wszystkie
  negatywne (2026-07-25).** Kafel `gemm_q8_0_dot4` (BM=BN=128, TM=8, TN=4, KB=2)
  daje 35-36 TOPS przy pulapie 97. Miks instrukcji na etap: 512 `v_dot4_i32_i8`,
  192 instrukcje epilogu skalowania, 144 `v_mov`, 48 `ds_read_b128` — czyli dot4
  to 60% wydawanych instrukcji.
  ZMIERZONE, CO ILE KOSZTUJE:
  - Epilog (cvt int32->f32 plus mnozenie przez skale aktywacji i wagi): **15%**
    (36 -> 42 TOPS po jego usunieciu). Jest strukturalnie minimalny — trzy
    operacje na akumulator na blok kwantyzacji i nie da sie ich sfaktoryzowac,
    bo skala aktywacji zmienia sie co 32 kolumny.
  - Nawet BEZ epilogu jest 42 z 97 TOPS, wiec glowny limiter jest gdzie indziej.
  ODRZUCONE PO POMIARZE (ksztalt kafla, wszystko na 4096x4096, T=1024):

  | kafel | wynik |
  |---|--:|
  | 128x128 TM8 TN4 KB2 (obecny) | **35 TOPS** |
  | 128x128 TM8 TN8 KB2 | 14 |
  | 128x128 TM8 TN8 KB1 | 18 |
  | 128x128 TM4 TN8 KB2 | 26 |
  | 256x128 TM8 TN8 KB2 | 14 |
  | 128x256 TM8 TN8 KB2 | 14 |

  Rozszerzony przeglad geometrii (ten sam ksztalt zadania):

  | kafel | watki | wynik |
  |---|--:|--:|
  | 128x128 TM8 TN4 KB2 (obecny) | 512 | **36 TOPS** |
  | 128x64 TM8 TN4 KB2 | 256 | 35 |
  | 64x128 TM8 TN4 KB2 | 256 | 34 |
  | 256x64 TM8 TN4 KB2 | 512 | 33 |
  | 192x128 TM8 TN4 KB2 | 768 | 26 |
  | 256x128 TM8 TN4 KB2 | 1024 | 24 |
  | dowolny ksztalt z KB=4 | — | 13-14 |

  Wynik trzyma sie 34-36 TOPS w szerokim pasmie geometrii i zalamuje dopiero
  poza nim — to sygnatura zasobu, ktory sie wysyca niezaleznie od strojenia,
  a nie zle dobranego kafla. Dwa ograniczenia dzialaja jednoczesnie:
  - **Miks instrukcji**: dot4 to 56% wydawanych instrukcji, wiec sufit przy tym
    rozkladzie to ~54 TOPS, nie 97.
  - **Pamiec globalna**: przy BM=128 macierz wag jest czytana 8 razy (142 MB na
    GEMM 4096x4096 przy T=1024), czyli 149 GB/s z ~220 GB/s pulapu DRAM.
    Aktywacje (4 MB) mieszcza sie w Infinity Cache i nie licza sie do DRAM.
  Wieksze BM zmniejsza ruch wag, ale blok 1024-watkowy zajmuje tyle slotow fal,
  ze na WGP miesci sie tylko JEDEN i nic nie przykrywa bariery — netto gorzej.
  ODRZUCONE DODATKOWO: programowe potokowanie odczytow LDS (podwojny bufor
  rejestrowy, kolejny krok k ladowany przed policzeniem biezacego) — **34 wobec
  35 TOPS**, bo latencja LDS jest juz ukryta zajetoscia; assembler potwierdza
  wzorzec 3x ds_read -> s_waitcnt -> 32x dot4, ale ten stall nic nie kosztuje.
  WNIOSEK: kafel jest w praktycznym optimum dla RDNA2. Dalszy zysk wymaga INNEJ
  DEKOMPOZYCJI (mniej powtorzonych odczytow wag bez wzrostu bloku), a nie
  kolejnego strojenia tego kernela.
- 🔴 **KOREKTA KOREKTY: zegar pamieci karty stoi na 456 z 1000 MHz, a nie
  Infinity Cache (2026-07-26).** Wczesniej „poprawilem” pomiar 386 GB/s na 226,
  obwiniajac granice Infinity Cache. To bylo BLEDNE wyjasnienie. Prawdziwa
  przyczyna: pod obciazeniem karta trzyma rdzen na pelnym boostcie 2545 MHz, ale
  PAMIEC na poziomie DPM 1 z 4 — 456 MHz z dostepnych 1000 (`pp_dpm_mclk`,
  probkowane z potwierdzeniem, ze proces zyje; pobor 173 W).

  | mclk | teoretycznie | zmierzone | sprawnosc |
  |---|--:|--:|--:|
  | 456 MHz (stan obecny) | 233 GB/s | 220-226 | 96% |
  | 1000 MHz (pelny) | 512 GB/s | 386-395 | 76% |

  Oba pomiary byly wiec POPRAWNE, tylko przy roznych stanach zegara. Skutki:
  - **Wyniki decode z tej sesji sa mieszanka dwoch stanow i nieporownywalne.**
    Ten sam kod, ten sam model: qwen Q8_0 277,5 -> 182,1 tok/s, Mistral Q4_K
    67,0 -> 40,0 tok/s. To NIE regresja kodu — to spadek zegara pamieci.
  - Prefill jest prawie nietkniety (Mistral 1734 -> 1695, qwen ~14 900 -> 14 561),
    bo jest ograniczony obliczeniami, a `sclk` boostuje normalnie. To wlasnie ta
    asymetria zdradzila przyczyne.
  - Wniosek „kafle int8 sa w praktycznym optimum” byl wyprowadzany m.in. z tego,
    ze GEMM zjada 149 z 220 GB/s. Przy pelnym zegarze ten udzial spada do 39%,
    wiec czesc plateau moze byc artefaktem polowy pasma. Analize trzeba powtorzyc
    po przywroceniu zegara, ZANIM uzna sie ja za zamknieta.
  CO Z TYM ZROBIC: `pp_dpm_*` i `power_dpm_force_performance_level` sa tylko do
  zapisu dla roota, wiec wymuszenie nalezy do operatora:
  `echo high | sudo tee /sys/class/drm/card1/device/power_dpm_force_performance_level`
  (card1 = 0x73bf = Navi 21; card0 to iGPU Raphael). Podejrzenie co do przyczyny
  zatrzasniecia: liczne bledy GPU z tej sesji (SIGSEGV w sterowniku, uszkodzenie
  sterty przy rozbiorce) mogly zostawic memory DPM w stanie zdegradowanym.
  ZASADA NA PRZYSZLOSC: kazdy pomiar pasma i decode musi zapisywac stan
  `pp_dpm_mclk`, inaczej liczby nie sa porownywalne miedzy przebiegami.
- 🔴 **KOREKTA: „386 GB/s pasma" było ZAWYŻONE — realny stream to ~226 GB/s
  (2026-07-25).** Benchmark rooflinu czytał bufor 256 MiB, czyli DOKŁADNIE na
  granicy 128 MB Infinity Cache, więc mierzył mieszankę cache i DRAM i dawał
  niestabilne 386-395. Zmierzone na buforze zapisanym wcześniej, po rozmiarach:

  | rozmiar bufora | przepustowość |
  |---|--:|
  | 64 MiB | 1 199 GB/s |
  | 128 MiB | 1 486 GB/s |
  | 256 MiB | 225 GB/s |
  | 512 MiB | 226 GB/s |
  | 1 GiB | 220 GB/s |

  Powyżej Infinity Cache karta daje **~226 GB/s** i jest to niezależne od wzorca:
  odczyty 8/16/32 B na wątek oraz pętla grid-stride mieszczą się w 219-226.
  KONSEKWENCJA: wszystkie wcześniejsze wnioski typu „jesteśmy na X% pasma" liczone
  wobec 386 były zaniżone. To także wyjaśnia, czemu pojedyncze kernele mierzone na
  małych macierzach pokazywały 900-960 GB/s — mieściły się w cache.
- 📊 **Gdzie naprawdę idzie decode Bielika (2026-07-25).** Rozbicie zmierzone,
  nie oszacowane:
  - Sama rodzina gemv NVFP4 na kształtach warstw i PEŁNYCH 4 GB wag (bez pomocy
    cache): **20 ms na token, 192 GB/s** — czyli 48 tok/s, gdyby nie było nic
    więcej. Realny decode to 28,7 ms (34,8 tok/s).
  - Ten sam pomiar dla Q4_K (Mistral): 18 ms, 212 GB/s. Czyli kernel NVFP4 jest
    tylko ~10% wolniejszy od Q4_K, a NIE dwukrotnie — wcześniejsze podejrzenie o
    słabą ścieżkę NVFP4 było błędne.
  - Kernel NVFP4 w streamingu: 218 GB/s wobec 226 pułapu, czyli **97%**. Sam
    odczyt wag bez jakiejkolwiek matematyki daje 219 GB/s, więc kernel jest
    ograniczony pamięcią, a nie dekodowaniem.
  ODRZUCONE PO POMIARZE (żadne nie pomogło): unroll 2/4 niezależnych ładowań
  (219-222), ładowania 16 B zamiast 8 B (213-219), aktywacja wniesiona do LDS
  (218), arytmetyczne rozpakowanie e2m1 zamiast LUT w LDS (bez różnicy),
  ciągły przedział grup na lane zamiast lane-stride (**208 — gorzej**, bo liczy
  się koalescencja w obrębie jednej instrukcji, a nie ciągłość per wątek).
  Jedyny zmierzony zapas w kernelu: strumień skal kosztuje 13% (bez niego 246
  GB/s), ale każda próba przebudowy dostępu do skal wyszła gorzej.
  ZOSTAJE DO ZBADANIA: ~30% kroku decode Bielika leży POZA gemv-ami projekcji, a
  Mistral tego narzutu nie ma. Grafy są przechwytywane w obu przypadkach
  (sprawdzone licznikiem instancjacji), więc to nie narzut wywołań. Bez profilera
  (na tej maszynie nie ma rocprof) nie da się tego przypisać, a zgadywanie
  kosztowałoby czas GPU bez gwarancji.
- ✅ **BIELIK NVFP4 DZIAŁA NA RADEONIE (2026-07-25).** Model generuje poprawną,
  merytoryczną polszczyznę (Warszawa/Kraków/Łódź z trafnymi opisami), a cztery
  równoległe żądania przez `serve` też. Prefill 1 246 tok/s, decode 32,7 tok/s.
  Trzy rzeczy trzeba było dołożyć:
  1. **Kafel `gemm_nvfp4_dot4`.** Wartości e2m1 to same wielokrotności 0,5, więc
     PODWOJONE są całkowite (0..12, mieszczą się w int8) — NVFP4 wchodzi na
     `v_dot4_i32_i8` BEZ straty dokładności, a czynnik 2 pochłania skala grupy.
     Grupa wag ma 16 kolumn, a blok kwantyzacji aktywacji 32, więc podblok
     akumuluje dwie niezależne sumy int32 (tak samo jak Q6_K). 24 TOPS.
  2. **Wariant `out_f32` kafli.** `lm_head` Bielika jest nieskwantowany
     (`ignore: ["lm_head"]`), a batchowa głowa logitów zapisuje f32. Typ wyjścia
     jest teraz parametrem wszystkich pięciu rodzin. Dotyczyło to nie tylko
     Bielika — Mistral przy współbieżności >= 12 trafiłby w to samo.
  3. Naprawa dekodowania float8 (osobny wpis niżej — to była PRZYCZYNA pustego
     wyjścia mimo poprawnego prefillu).
  ZOSTAJE: decode 32,7 tok/s to 134 GB/s, czyli 35% pasma karty, gdy Mistral o
  tym samym rozmiarze osiąga 71%. Ścieżka gemv NVFP4 czyta wartości przez LUT w
  LDS, a szybka CT-S0 N64/K128 jest wyłączona (brak jej kafli na tej karcie).
- 🔴 **NVIDIA-owy inline asm KOMPILOWAŁ SIĘ CICHO NA AMD I LICZYŁ ŚMIECI
  (2026-07-25).** `_e4m3x2_to_f16x2` (dekodowanie skal `float8_e4m3`) to
  `inlined_assembly` z treścią PTX. Na gfx1030 kompilator zgłasza tylko
  `unknown asm constraint 'h'` i **buduje kernel dalej**, z pominiętą
  instrukcją: zamiast 1,5 wychodziło 1,1e-44. Zmierzone na karcie na ośmiu
  kodach. Skutek: WSZYSTKIE kernele gemv NVFP4 i ścieżka KV fp8 liczyły na tej
  karcie śmieci — Bielik generował pustkę mimo poprawnego prefillu.
  Dlaczego to nie wyszło wcześniej: kernele z `mma` uczciwie NIE kompilują się
  (błąd doboru instrukcji), więc cały czas zakładałem, że niezgodny inline asm
  po prostu wypada z katalogu. Konwersja zachowuje się inaczej niż mma.
  Naprawa: `f8e4m3_to_f32` i `f8e4m3x2_to_f16x2` w `arch_dot` jako JEDYNE
  źródło, używane przez `gemv2` i `kv_fp8`. Wariant przenośny składa wzorzec
  bitowy f32 (bias 7 -> 127, mantysa << 20) z osobną gałęzią dla subnormalnych.
  ZABEZPIECZENIE: builder katalogu traktuje teraz `unknown asm constraint` jak
  BŁĄD, bo inaczej ta klasa usterki wychodzi dopiero na wyniku modelu.
  Audyt pozostałych `inlined_assembly`: `mma.sync` w `gemm.mojo`,
  `gemm_fp8.mojo` i `tensor_core_i8.mojo` — wszystkie w kernelach, których na
  gfx1030 NIE MA w katalogu, więc nie przeciekły.
- ✅ **Liczba wątków bloku wynika teraz z wymiarów kafla (2026-07-25).**
  Trzy wpisy „128x64" w dyspozytorze Rusta przekazywały 512 wątków, a kernele są
  zbudowane na 256 ((BM/TM)*(BN/TN)). Nadmiarowe wątki adresowały poza kafel w
  LDS, więc Mistral-7B Q4_K dawał INNE tokeny w kolejnych powtórzeniach.
  Wyłapała to bramka powtarzalności w `forge bench` (`greedy token IDs differ
  between benchmark repetitions`) — nie test jednostkowy kernela, bo tam
  `block_dim` liczyłem z tych samych parametrów co instancję, więc niezgodność
  nie mogła wystąpić. Wniosek: to nie jest błąd do naprawienia jednorazowo, tylko
  klasa błędu do wyeliminowania — `DotTile` trzyma BM/BN/TM/TN i sam wylicza
  siatkę oraz blok, więc ręczna wartość nie ma już gdzie się rozjechać.
  Zasięg: q4_k i q6_k trafiały tam ZAWSZE (stąd objaw), q8_0 i f16 tylko dla
  65-128 tokenów, czyli poza mierzonymi kształtami — dlatego wcześniejsze wyniki
  qwen zostają ważne i po naprawie nie zmieniły się (14 900 wobec 14 914).
- ✅ **Kafel GEMM bez jednostki macierzowej — `src/gemm_dot.mojo` (2026-07-25).**
  Rodzina `gemm_*` w `gemm.mojo` stoi na kontrakcie fragmentów `mma.m16n8k16` +
  `ldmatrix`. Tego NIE DA SIĘ sensownie emulować: rozkład fragmentów rozrzuca
  A i B po lane'ach tak, że wątek nie ma danych na własny wynik, więc emulacja
  wymagałaby instrukcji cross-lane droższych niż zysk. Zamiast tego zmieniona
  jest DEKOMPOZYCJA: wątek trzyma własny kafel wyjścia TM x TN w rejestrach i
  czyta swoje wiersze z LDS. Trzy rzeczy zdecydowały o wydajności:
  1. **Układ LDS parami/czwórkami k, wiersze ciągłe** (`tile[k/2][row][k%2]`).
     Przy układzie wiersz-major lane czytał 16 B ze skokiem 320 B, czyli fala
     trafiała w 8 z 32 banków. Po zmianie fala czyta jeden ciągły blok:
     **14 -> 24 TFLOPS** na tej samej matematyce. To był największy pojedynczy
     zysk w całej pracy nad tym kernelem.
  2. **>= 8 niezależnych akumulatorów** (wymóg z rooflinu, patrz niżej).
  3. **Ręczne podwójne buforowanie** — RDNA2 nie ma `cp.async`, więc kafel
     następnego etapu jedzie najpierw do rejestrów.
  Zmierzone (gfx1030, 4096x4096, T=1024, wszystkie warianty przechodzą kontrolę
  poprawności wobec referencji hosta):

  | kernel | najlepszy kafel | wynik | pułap karty |
  |---|---|--:|--:|
  | `gemm_f16_dot2` | 128x128 / 256 wątków | 23 TFLOPS | 49 TFLOPS |
  | `gemm_q8_0_dot4` | 128x128 / KB=2 | 35 TOPS | 97 TOPS |
  | `gemm_q4_k_dot4` | 128x64 / KB=2 | 32 TOPS | 97 TOPS |
  | `gemm_q6_k_dot4` | 128x64 / KB=2 | — | 97 TOPS |

  Formaty kwantyzowane różnią się TYLKO rozpakowaniem wag i członem korekty:
  Q8_0 jest symetryczne (czysty iloczyn int32), Q4_K asymetryczne
  (`w = d*sc*q - dmin*m`, więc drugi człon bierze `xsm` z `quantize_act_q8_1`),
  a Q6_K ma stałe przesunięcie -32, które stosujemy JUŻ PRZY ZAPISIE do LDS —
  dzięki temu iloczyn skalarny nie potrzebuje członu z sumą aktywacji. Q6_K ma
  też jedną skalę na 16 kolumn wobec 32-kolumnowego bloku aktywacji, więc
  akumuluje dwie niezależne sumy int32 na podblok.
  Q6_K jest konieczny, a nie opcjonalny: `Q4_K_M` (i każdy inny wariant K)
  trzyma część tensorów w Q6_K, więc bez niego prefill takiego modelu na AMD
  przerywa się na `kernel not loaded: gemm_q6_k_f16_bm64`.

  Pułapka, którą złapałem na sobie: kafel 192x128 przy 768 wątkach wychodził
  „najszybszy" (26 TFLOPS), bo `W_PASSES = BN/(NT/4)` dawało ZERO i kernel wcale
  nie wnosił wag. Staging jest teraz uogólniony na kafle węższe niż jedno
  przejście, a każdy mierzony wariant ma kontrolę poprawności — bez tego
  benchmark mierzy nieprawidłowy kernel i nikt tego nie widzi.
- ✅ **Wybór backendu GPU w czasie działania (2026-07-25).** `forge_hal::gpu`
  jest jedynym miejscem, które wie, czy pod spodem jest CUDA czy HIP: pyta
  sterowniki o urządzenie i zwraca `Arc<dyn Device>`; `FORGE_DEVICE=cuda|hip`
  przypina wybór. CLI nie konstruuje już `CudaDevice` w sześciu miejscach.
  Jedna binarka obsługuje obie karty, bo cudarc ładuje sterownik dynamicznie.
- ✅ **Katalog kerneli publikuje częściowy zestaw dla architektury w porcie
  (2026-07-25).** `FORGE_KERNEL_BUILD_PARTIAL=1` zapisuje to, co się kompiluje,
  plus listę `unsupported.txt`. Brakujący kernel zgłasza się przy uruchomieniu
  (`kernel not loaded: <nazwa>`), więc niepełny zestaw jest widoczny, a nie
  cichy. Dla gfx1030: **347 z 481 kerneli**.
- ✅ **Attention prefill wybiera ścieżkę po DOSTĘPNOŚCI, nie po producencie
  (2026-07-25).** Flash-attention w Mojo stoi na `mma`, więc na gfx1030 nie ma
  go w katalogu; domyślny wybór sprawdza teraz artefakt i schodzi na przenośny
  kernel skalarny (`AttnBackend::Scalar`), który jest zarazem bitową referencją.
  To był ostatni brakujący element, żeby prefill przeszedł end-to-end.
- ✅ **CLI nie zatrzymywało wątku roboczego silnika — na ROCm kończyło się to
  uszkodzeniem sterty (2026-07-25).** `forge bench` i `forge run` po wypisaniu
  wyników po prostu wychodziły, porzucając `EngineHandle`. Wątek roboczy trzyma
  model i zasoby urządzenia, więc proces kończył się w chwili, gdy ten wątek
  jeszcze je zwalniał, a sterownik GPU był już w trakcie własnej rozbiórki
  (`double free or corruption` z wnętrza `libamdhip64`, SIGABRT w wątku
  `forge-engine-worker`). `EngineHandle::shutdown` istniał i był wołany WYŁĄCZNIE
  z testów. Pierwsza poprawka dodała go do `bench` i `run`, ale to za mało:
  wyjście przez `?` (np. brakujący kernel) omija wywołanie i objaw wracał.
  Trwała wersja zatrzymuje wątek w `Drop` uchwytu — `tx` jest w `Option`, żeby
  `Drop` mógł rozłączyć kanał PRZED dołączeniem wątku (inaczej join czekałby na
  wątek widzący żywego nadawcę). Teraz KAŻDA ścieżka wyjścia jest czysta, także
  `serve` i wyjścia błędne.
  Diagnoza była myląca, bo objaw wyglądał na błąd backendu HIP: ślad miał tylko
  ramki ROCm i pojawiał się także wtedy, gdy przebieg kończył się wcześniej
  błędem. Rozstrzygnęło to, że abort leciał w wątku roboczym PO wypisaniu
  wyników. Na CUDA ta sama luka jest tylko niewidoczna, nie nieszkodliwa.
- ✅ **DWA BŁĘDY CZASU ŻYCIA W BACKENDZIE HIP — obie ścieżki były SIGSEGV
  (2026-07-25).** Silnik wywracał się na PIERWSZYM uruchomieniu kernela, w
  środku `hipModuleLaunchKernel`. Przyczyna: `KernelHandle` nie trzymał modułu,
  a rejestr kerneli przechowuje TYLKO uchwyty — `Drop` modułu robił
  `hipModuleUnload` natychmiast po pobraniu uchwytu, więc każdy launch szedł po
  wskaźniku do zwolnionego kodu. Na CUDA problemu nie było, bo cudarc trzyma
  moduł wewnątrz uchwytu funkcji. Naprawa: `ModuleImpl::kernel` przyjmuje
  `self: Arc<Self>`, a `HipKernel` zatrzymuje `Arc<HipModule>`. Drugi, pokrewny:
  moduł trzyma teraz WŁASNĄ kopię obrazu code objectu, bo `hipModuleLoadData`
  nie gwarantuje skopiowania bufora wołającego. Test `mojo_artifact_launches`
  pilnuje obu (uruchamia kernel z artefaktu Mojo po porzuceniu `Module`, z wątku
  roboczego) — kontrakt generator artefaktów <-> backend ma własny test, bo
  wcześniejszy test z `hipcc` przechodził i niczego nie wyłapał.
- ✅ **Roofline gfx1030 zmierzony w Mojo; ILP okazało się warunkiem pomiaru
  (2026-07-25).** Pierwsze mikrobenchmarki miały po cztery niezależne łańcuchy
  akumulacji i pokazywały DOKŁADNIE POŁOWĘ pułapu karty — VALU RDNA2 ma
  kilkutaktową latencję wyniku, więc przy czterech łańcuchach pomiar jest
  latency-bound. Po podniesieniu do ośmiu (sweep 2/4/8/16 potwierdził wypłaszczenie
  na ośmiu) liczby są następujące:

  | pomiar | 4 łańcuchy | **8 łańcuchów** | teoretyczny szczyt |
  |---|--:|--:|--:|
  | odczyt DRAM (1 GiB) | — | **221 GB/s** | ~512 GB/s |
  | odczyt z Infinity Cache (64 MiB) | — | **1 596 GB/s** | — |
  | FP32 FMA | 18 TFLOPS | **25 TFLOPS** | ~23 TFLOPS |
  | f16 `v_dot2_f32_f16` | 25 TFLOPS | **49 TFLOPS** | ~46 TFLOPS |
  | int8 `v_dot4_i32_i8` | 50 TOPS | **97 TOPS** | ~92 TOPS |

  To jest jednocześnie WYMAGANIE PROJEKTOWE dla każdego kernela na tej karcie:
  mniej niż osiem akumulatorów na wątek to najwyżej połowa maszyny, niezależnie
  od jakości kafla. Obie jednostki dot (`v_dot2_f32_f16` dla f16 i
  `v_dot4_i32_i8` dla int8) dają f32/i32 akumulację przy podwójnej przepustowości
  odpowiedniego FMA i są jedyną drogą do pułapu bez jednostki macierzowej.
  ROZBICIE CELU wobec llama.cpp na ROCm (Mistral-7B Q4_K, 7,25e9 parametrów):
  - **decode 79,2 tok/s = 346 GB/s.** To WIĘCEJ niż zmierzone pasmo strumieniowe
    (226 GB/s) — patrz wpis o Infinity Cache niżej; oznacza to, że przy 4 GB wag
    część ruchu i tak trafia w cache, a syntetyczny pomiar strumienia jest dolnym
    oszacowaniem. Tak czy inaczej llama.cpp jest w decode blisko sprzętu.
  - **prefill 1 406 tok/s = 20,4 TOPS efektywnie = 21% zmierzonego pułapu int8.**
    To jest **~5x zapasu**, nie 2,5x jak wynikało z latency-bound pomiaru.

  WNIOSEK: cała okazja na RDNA2 leży w PREFILLU, a dźwignią jest int8 dot4, nie
  fp16 — llama.cpp zostawia tę jednostkę w większości niewykorzystaną. Prefillowy
  GEMM budujemy więc wokół `v_dot4_i32_i8` (dequant Q4_K/Q8_0 do int8 plus
  kwantyzacja aktywacji do int8), a nie wokół dekwantyzacji do fp16; dla modeli
  f16 bez kwantyzacji analogiczną rolę pełni `v_dot2_f32_f16`. Ta sama matematyka
  jest przenośna na RDNA3/RDNA4, gdzie dodatkowo dochodzi WMMA.
  Decode zostaje przy kernelach bandwidth-bound i tam wystarczy nie zepsuć.
- ✅ **Pule VRAM i grafy w backendzie HIP (2026-07-25).** `HipDevice::new`
  przyjmuje `PoolSizes` i zajmuje z góry trzy pule z tą samą polityką co CUDA:
  bump dla wag, slab dla KV, pierścień dla aktywacji. `PoolSizes` przeniesione
  z `cuda.rs` do `lib.rs` z re-eksportem, więc 38 istniejących ścieżek
  `forge_hal::cuda::PoolSizes` działa bez zmian. Wyczerpanie puli zwraca
  typowany `OutOfMemory` (ta sama poprawka co w CUDA, żeby admission i tiering
  go rozpoznały). Grafy działają przez `hipStreamBeginCapture` /
  `hipStreamEndCapture` / `hipGraphInstantiate` / `hipGraphLaunch`, więc
  `supports_graph_capture` raportuje już `true`. Test `hip_backend` **5/5**:
  capability, roundtrip pamięci, ładowanie code objectu z uruchomieniem,
  semantyka trzech aren (bump nie oddaje, pierścień oddaje po
  `reset_activations`, slab zgłasza typowany OOM) oraz przechwycenie i dwa
  odtworzenia grafu z kontrolą wyniku.
- ✅ **llama.cpp na ROCm jako punkt odniesienia dla RDNA2 (2026-07-25).**
  Zbudowany z `-DGGML_HIP=ON -DAMDGPU_TARGETS=gfx1030`. PUŁAPKA: bez
  `HIP_VISIBLE_DEVICES=0` llama.cpp widzi DWA urządzenia ROCm — dGPU i iGPU
  Raphael (gfx1036) — próbuje rozłożyć model na oba i **zrzuca pamięć**.
  Zmierzone (6900 XT, `-ngl 99`, build `112c7815`). Pierwszy pomiar był na
  pp512/tg64, ale `forge bench` generuje prompt 1024-tokenowy, więc punkt
  odniesienia powtórzyłem na pp1024/tg128 — krótszy prompt zawyża prefill
  (10 974 wobec 7 827 na tym samym modelu), a porównywać wolno tylko ten sam
  kształt:

  | model | prefill pp1024 | decode tg128 | prefill efektywnie |
  |---|--:|--:|--:|
  | qwen3-0.6B Q8_0 | 7 827,3 tok/s | 239,8 tok/s | 9,3 TOPS |
  | qwen3-0.6B Q5_K_M | 7 040,9 tok/s | 260,7 tok/s | 8,4 TOPS |
  | Mistral-7B Q4_K_M | 1 300,6 tok/s | 79,0 tok/s | 18,9 TOPS |

  Uwaga do celu: mały model jest DALEJ od pułapu karty (9,3 z 97 TOPS) niż duży
  (18,9), bo jego GEMM-y są małe i dominuje narzut. Łatwiej więc pobić llama.cpp
  w TOPS-ach na qwen0.6B, ale trudniej wycisnąć z karty procent pułapu.

  Porównanie z RTX 4090 (pp1024/tg128, więc orientacyjne): prefill jest **5,6x
  gorszy** na qwen0.6B i **9,0x** na Mistralu, a decode tylko **2,7x** i
  **2,3x**. To dokładnie profil karty bez jednostek macierzowych: decode jest
  ograniczony pasmem (512 wobec 1008 GB/s, czyli ~2x), a prefill traci na braku
  MMA. WNIOSEK DLA CELU: realny target FORGE na RDNA2 to parytet decode z
  llama.cpp (79 tok/s Mistral, 241 qwen0.6B), a prefill będzie trudną częścią.
- ❌ **vLLM nie wchodzi na tę kartę (2026-07-25).** Lokalny obraz
  `vllm/vllm-openai:latest` to build CUDA (`torch 2.11.0+cu130`, `torch.version.hip`
  = None), więc na ROCm nie uruchomi się w ogóle. Wariant `rocm/vllm` celuje w
  CDNA (gfx90a/gfx942); RDNA2 nie jest wspierana. Do porównań na tej karcie
  zostaje llama.cpp.
- 🟡 **Backend HIP/ROCm — pierwszy pionowy przekrój działa (2026-07-25).**
  Maszyna zmieniła kartę na **Radeon RX 6900 XT (gfx1030, RDNA2)**, więc FORGE
  przestał startować (`CudaContext::new(0): CUDA_ERROR_NO_DEVICE`). Zweryfikowane
  o sprzęcie i toolchainie: 80 CU, wavefront **32**, **16,0 GB VRAM**, ROCm 7.2.4,
  brak sterownika NVIDIA. **Mojo działa na tej karcie** (`gpu_smoke` policzył
  kernel), `arch_name()` zwraca `gfx1030`, a `dump_asm` daje assembler AMDGCN
  z TYM SAMYM zmangowanym symbolem co build PTX — czyli format manifestu i
  `out_dir = build/<arch>` przenoszą się bez zmian. Łańcuch artefaktu
  potwierdzony do końca: `.s` → `clang -x assembler -target amdgcn-amd-amdhsa
  -mcpu=gfx1030` → HSACO (ELF DYN, EM_AMDGPU) z symbolem jądra i `.kd`.
  Pułapka: Mojo emituje `.amdgcn_target "...-unknown-gfx1030"`, a clang wymaga
  wariantu bez `unknown` — jeden `sed`, ta sama klasa łatki co obecne podbicie
  `.version 8.1 → 8.4` dla FP8.
  ROZSTRZYGNIĘCIE O SPRZĘCIE: gfx1030 **nie ma jednostek macierzowych**
  (`__builtin_amdgcn_wmma_*` → „needs target feature gfx11-insts"; gfx1100 jako
  kontrola kompiluje się). Ma natomiast int8 dot (`v_dot4_i32_i8`). Więc wszystkie
  kernele oparte na `mma`/`ld_matrix` wymagają wariantów dot/VALU, a ścieżki FP8
  są na RDNA2 niemożliwe. WMMA wraca na RDNA3, FP8 na RDNA4.
  ZROBIONE: `forge-hal` ma cechę `hip` i backend `src/hip.rs` — wykrycie
  urządzenia z capability, pamięć (device + pinned host), streamy, eventy
  (także z pomiarem czasu), kopie D2D, `hipModuleLoadData` i
  `hipModuleLaunchKernel`. Właściwości czyta shim w C kompilowany przez
  `build.rs` clangiem z ROCm, żeby układ `hipDeviceProp_t` i numeracja enumów
  były rozstrzygane przez kompilator, nie zgadywane w Ruście. Test
  `hip_backend` 3/3: capability, roundtrip pamięci z kopią D2D i kontrolą
  zakresu, oraz ładowanie code objectu zbudowanego w locie przez
  `hipcc --genco` z uruchomieniem kernela i porównaniem wyniku.
  Cecha jest opt-in, więc build CUDA-only jest nietknięty (workspace kompiluje
  się bez ROCm).
  ŚWIADOMIE JESZCZE NIE: areny pul (alloc idzie wprost przez `hipMalloc`,
  `reset_activations` zwraca `Unsupported`) i grafy (`supports_graph_capture`
  raportuje `false`, więc warstwy wyżej ich nie użyją). To następny krok —
  bez pul silnik nie wystartuje na AMD.
  NIESPODZIANKA DO ZAPAMIĘTANIA: na RDNA `multiProcessorCount` liczy **WGP**,
  nie CU — 6900 XT zgłasza 40, a `rocminfo` 80. Heurystyki „bloki na SM"
  trzeba czytać przez ten pryzmat.
- ✅ **Kopie D2D w mixerze DeltaNet usunięte: Qwen 27B do 112,3 tok/s
  (2026-07-25).** Batchowane projekcje zostawiały cztery kopie D2D na lane na
  warstwę (1536 na krok przy B=8) do jednotokenowego scratchu. Konsumenci
  czytają teraz swój wiersz przez przesunięcie bajtowe: `conv_silu` i
  `log_decay` miały już warianty `_at`, doszły `deltanet_beta_sigmoid_f32_at`
  i `deltanet_gated_rmsnorm_f16_at`. Cztery jednotokenowe bufory
  (`qkv_mixed`, `z`, `alpha`, `beta_raw`) w `HybridBufs` straciły użycia i
  zostały usunięte wraz z alokacjami. Pomiar: C=8 105,2 → **108,8** (+3,4%),
  C=16 109,0 → **112,3** (+3,0%), C=1 40,0 → 40,5 bez regresji.
  Narastająco od parowania B=2: **50,8 → 112,3 tok/s przy C=16, czyli 2,21x**.
  Bramki: workspace 606/606, `hybrid_state_pool_gpu` 32/32, koherencja i
  self-konsystencja przy ośmiu równoległych requestach.
- 🟡 **Mixed prefill+decode: hipoteza „nie włącza się dla Bielika" BŁĘDNA;
  polityka kwantu poprawiona, regresja p1024 otwarta (2026-07-25).** Czytałem
  25,9% czasu GPU w kernelach prefillu (profil decode-only C=32) jako niewtopiony
  prefill. To był błąd: `mixed_step_capable` przepuszcza Bielika (głowa F16,
  gęsty, bez tieru), a te kernele TO są kroki mieszane, które jednocześnie
  posuwają decode. A/B `FORGE_MIXED_STEP`: in32/o256 **2 683 wobec 2 537 tok/s**
  (mixed pomaga 5,7%, TTFT 95,7 wobec 106,5 ms), ale p1024/o128 **780 wobec 824**
  (mixed SZKODZI 5,4%, TTFT 218,5 wobec 228,3 ms).
  Poprawione po drodze: `mixed_gpu_group` skracał kwant o wiersze decode ZAWSZE
  (`take = quantum - b`), więc prompt o długości 993..1024 dostawał chunk 992 i
  wymagał drugiego, prawie pustego przejścia po wagach (~11 ms) zamiast jednego
  kafla GEMM więcej (~6% jednego przejścia). Teraz kwant skraca się tylko wtedy,
  gdy prompt i tak nie zmieści się w jednym chunku. Zakres poprawki jest wąski
  (stara ścieżka i tak brała całość przez `.min(pending)`), więc w pomiarze
  z promptami ~1030 tokenów jest neutralna — 2 720/775 wobec 2 683/780.
  NIEWYJAŚNIONE: skąd 5,4% straty przy p1024. Do czasu diagnozy jest to jawny
  lever: workloady zdominowane długimi promptami zyskują ~5% na
  `FORGE_MIXED_STEP=0`, kosztem 4% TTFT. NIE dodawałem heurystyki wyłączającej
  mixed po długości promptu — to byłoby strojenie bez zrozumienia przyczyny.
- ❌ **Natywne FP4 w prefillu GGUF: ZMIERZONE I ODRZUCONE (2026-08-09).** GB10
  retiruje `mma...kind::mxf4nvf4.block_scale` w tym samym czasie co `m16n8k16.f16`
  (4,358 wobec 4,350 ms, sonda `mma_rate`), więc cztery bity to 504,5 wobec
  126,4 TFLOP/s — czterokrotność, bo `k` jest 64 zamiast 16. Kernel
  `gemm_nvfp4_mma_f16` bierze wagi WPROST z bloków GGUF i na kształtach
  ThinkingCap 27B jest 1,07-1,58x szybszy od `gemm_nvfp4_gguf_f16` (bench
  `gemm_fp4_bench`, kwantyzacja aktywacji wliczona). ALE instrukcja wymaga
  czterech bitów PO OBU STRONACH: aktywacja schodzi do NVFP4 i kosztuje
  **14,18% rozpiętości wyniku** (`gemm_fp4.rs`), przy błędzie samego kafla
  0,04%. To nie jest zaokrąglenie, tylko inny model — 20% prefillu nie jest tego
  warte. Prawdziwy zapas leży gdzie indziej: ten sam GEMM osiąga 57 TFLOP/s przy
  suficie f16 126 TFLOP/s i rusza 31 GB/s przy paśmie 237, więc nie jest ani
  compute-, ani memory-bound — ogranicza go coś w kaflu, a `ncu` na tej maszynie
  nie ma.
- ❌ **Głowa logitów FP8 w batchowym decode: ZMIERZONA I ODRZUCONA
  (2026-07-25).** Profil B=32 pokazał głowę F16 jako 5% czasu GPU przy 465 GB/s
  (262 MB na krok), a paczka `fp8_lm_head` już istnieje — kusząco. Implementacja
  przeszła: `gemm_fp8_impl` sparametryzowany typem wyjścia, trzy instancje
  `gemm_fp8_out_f32{,_bm64,_big}`, wspólne ciało `gemm_fp8_family` dla obu
  rodzin (bez duplikacji kwantyzacji aktywacji) i routing w `logits_gemm`.
  Zysk: 0,56 → około 0,28 ms z 8,3 ms kroku, czyli 3,4%.
  POWÓD ODRZUCENIA: `batched_reproduces_golden` przechodzi przed zmianą i
  failuje po — głowa e4m3 ma 3-bitową mantysę w warstwie, która WPROST wybiera
  token, więc strumień greedy się przestawia. Jedyną „walidacją" byłoby
  przepisanie wzorcowych ID pod wersję gorszą numerycznie. Całość wycofana
  (kernele, rejestracje, routing, manifest wrócił do 474).
  LEPSZY CEL: głowa F16 czyta 262 MB w 563 us, czyli 465 GB/s — przy attention
  na 776 GB/s w tym samym kroku jest tam około 0,23 ms do wzięcia BEZ zmiany
  numeryki, samym strojeniem `gemm_f16_out_f32`.
  ZNALEZISKO PRZY OKAZJI: `logits_gemv` (ścieżka jednostrumieniowa) PREFERUJE
  `fp8_lm_head`, gdy paczki są zbudowane, a ścieżka batchowa używa F16 — czyli
  na Bieliku z auto-fp8 pojedynczy strumień ma logity e4m3, a batch nie.
  Wzorcowe ID w `batched_bielik` kodują zachowanie batchowe (F16). To realna
  niespójność: jakość zależy od współbieżności, tylko odwrotnie niż zakładałem.
  Do rozstrzygnięcia osobno — kandydatem jest odebranie FP8 głowie także w
  ścieżce jednostrumieniowej.
- ✅ **Dwa testy Bielika nie mogły lecieć równolegle (2026-07-25).**
  `batched_reproduces_golden` i `scheduler_prefill_p1024_o256_b1_b4_b8_b16`
  ładują pełnego Bielika z pulą wag 12 GB każdy, więc drugi
  `.expect("cuda device")` padał na VRAM. Osobno przechodziły, razem nie —
  serializuje je teraz mutex `BIELIK_GPU`, tak jak `PREPARED_Q8_SCRATCH`
  w `forge-kernels`. `cargo test -p forge-server --test batched_bielik --
  --ignored` daje 2/2.
- ✅ **Batchowane projekcje DeltaNet: Qwen3.6-27B 71 → 109 tok/s (2026-07-25).**
  Profil `nsys` przy B=8 pokazał, że `gemv_q8_0_dp4a` to **37,9% czasu GPU i
  1526 launchy na krok** — dokładnie 8 lane'ów x 64 warstwy x 3 projekcje
  (gate/alpha/beta), z dominującym `gate_proj` `[4096, 5120]` (~21 MB na warstwę
  czytane OSOBNO dla każdego lane'a, przy 857 GB/s czyli na rooflinie). Problemem
  była liczba przejść po wagach, nie kernel.
  Projekcje są bezstanowe, więc wiersze mogą pochodzić z różnych sekwencji:
  nowy `hybrid_delta_projections` liczy wszystkie cztery RAZ na warstwę dla n
  wierszy z `bb.x`, a `hybrid_delta_mixer(l, d, lane)` przenosi swój wycinek do
  jednotokenowego scratchu, z którego czytają kernele stanowe (conv, skan).
  `HybridBufs` ma teraz `batched_{qkv_mixed,z,alpha,beta_raw}` na `cap` wierszy
  i realokuje się, gdy batch cap wzrośnie. Ścieżka jednostrumieniowa to ten sam
  kod z n=1, bez drugiej implementacji.
  PUŁAPKA: `self.gemm(..., 1, ...)` NIE działa dla tych wag — ścieżka GEMM dla
  NVFP4 GGUF odrzuca jeden token (`gemm_nvfp4_gguf_f16 wymaga co najmniej dwóch
  tokenów`), co objawiło się HTTP 500 na każdym żądaniu. Dlatego `hybrid_project`
  rozgałęzia: `gemv` dla jednego wiersza, batchowy `gemm` dla wielu.
  Pomiar (RTX 4090, prompt 85, out 128, tokeny z `usage` serwera):
  C=1 40,4 → **40,0** (bez regresji), C=4 66,0 → **89,4** (1,35x),
  C=8 70,1 → **105,2** (1,50x), C=16 71,3 → **109,0** (1,53x).
  Narastająco od parowania B=2: **50,8 → 109,0 tok/s przy C=16, czyli 2,15x**.
  Single-stream z MTP bez zmian (111,7 tok/s, prefill 2 277). Bramki: cały
  workspace **606/606**, koherencja i self-konsystencja przy 8 równoległych
  requestach (cztery grupy identycznych promptów po jednym unikalnym wyjściu).
  Zostaje: kopie D2D wycinka per lane (4 na warstwę na lane) da się później
  usunąć, dodając offset lane do konsumentów (`deltanet_conv_silu`, `log_decay`,
  `beta_sigmoid`, `gated_rms`, `sigmoid_mul`).
- ✅ **Rozbieżność ścieżek decode naprawiona — dotyczyła WYŁĄCZNIE B=1
  (2026-07-25).** `batched_matches_single_seq` failował od dawna bez wyjaśnienia.
  Nowy `Model::read_single_logits` (symetryczny do `read_batch_logits`) i
  przeplatany porównywacz w teście pokazały, że rozbieżność tokenu w kroku 19 to
  SKUTEK: logity obu ścieżek różniły się o **rel_l2 1,1e-2 do 3,5e-2 od
  PIERWSZEGO kroku**, a token przewracał się dopiero gdy margines top-2 (0,0298)
  spadł poniżej szumu (max|delta| 0,33). Dwa rzędy za dużo na zaokrąglenia f16.
  Przyczyna: `gemm_rows` dla Q8_0 kierował **jeden token** na kafel
  `gemm_q8_0_i8mma_at` dopełniany do >=64 tokenów, który kwantyzuje aktywacje
  inaczej niż dp4a GEMV ścieżki serialnej. Nowy `gemv_q8_0_dp4a_f16_at`
  (odpowiednik istniejącego `gemv_q4_k_dp4a_f16_at`) daje okno wierszy, więc
  `n_tokens == 1` używa teraz DOKŁADNIE tego samego kernela co dekod
  jednosekwencyjny. Test przechodzi, a cały workspace jest zielony: **606/606**.
  KOREKTA wcześniejszego wpisu: twierdziłem, że dla B>1 zostaje kompromis
  jakość/przepustowość — to było błędne. Szerokości 2/4/8/16 już wcześniej
  szły na weight-stationary `gemm_q8_0_i8mma_b*`, czyli tę samą kwantyzację
  aktywacji co ścieżka serialna; różnił się tylko B=1, i to przy zerowym zysku
  wydajności (kafel 64-tokenowy dla jednego tokena). Jakość wyjścia NIE zależy
  od obciążenia serwera. Bez regresji: Bielik decode-only 158/1720/2701 tok/s
  przy C=1/16/32 (przed: 158/1687/2746), single-stream Mistral 14 781 prefill /
  171,1 decode i qwen3-0,6B 58 390 / 671,8 bez zmian.
- ✅ **Hybrydowy decode: grupy zamiast par, +38% na Qwen3.6-27B (2026-07-25).**
  Scheduler dzielił kolejkę hybrydową na `chunks_exact(2)` i wykonywał krok B=2
  na parę, więc osiem aktywnych sekwencji dawało dwa tokeny na krok i
  przepustowość stała ~51 tok/s niezależnie od współbieżności. Samo
  `record_hybrid_batch_forward` było już generyczne po `n` (mixery serialne per
  lane, bo pula stanów aktywuje jeden lease naraz i ich scratch jest
  jednotokenowy; norm, FFN i głowa logitów batchowane) — blokadą był wyłącznie
  jawny warunek `n != 2` plus parowanie w schedulerze. Oba zniesione:
  `hybrid_batch_capable` (dawniej `..._b2_capable`), `ensure_batch(max_active)`
  zamiast `ensure_batch(2)`, a grupy dobiera `hybrid_group_size`.
  KLUCZOWE: szerokość grupy musi trafiać w kernel. Batchowe GEMM-y NVFP4 GGUF
  miały strojony wariant `_nvidia` dla b3/b4/b8, ale NIE dla b16 — więc każda
  grupa 9..16 spadała na kernel przenośny. Zmierzone na RTX 4090 (prompt 85,
  out 128): grupa 8 dawała **70,8 tok/s**, a grupa 10 albo 16 — **37,5** i
  **39,5**. Brakująca instancja to jedna linia (`gemm_nvfp4_gguf_f16_b16_nvidia
  = gemm_nvfp4_gguf_f16_nvidia_impl[16]` — szablon był już generyczny po
  batchu), 64 rejestry, zero spilli. Po jej dodaniu klif zniknął: grupa 10 daje
  **67,8**, grupa 16 — **71,3**, więc `hybrid_group_size` to po prostu
  `pending.min(16)` bez zaokrąglania do potęgi dwójki. Powyżej 16 dispatch
  przechodzi na kafel MMA bm32, niezmierzony dla tej ścieżki — tam jest granica.
  Wariant `_nvidia` b16 zyskuje też każda inna ścieżka trafiająca w bucket
  9..16 (weryfikator MTP, chunki prefillu hybrydy).
  A/B tym samym klientem (tokeny liczone z `usage` serwera, nie tokenizerem
  klienta): C=1 40,3→40,4 (bez regresji), C=4 50,9→**66,0** (1,30x),
  C=8 50,8→**70,1** (1,38x), C=16 50,8→**71,3** (1,40x). Pełna krzywa po
  naprawie: C=6 69,1 · C=8 70,1 · C=10 67,8 · C=12 68,9 · C=16 71,3 — płaska,
  zamiast przyklejonej do 51. Mediana opóźnienia przy C=16 spadła z 51,8 s do
  28,7 s. Bramki: `hybrid_state_pool_gpu` 32/32, `forge-engine` 140/140,
  `forge-kernels` golden 70/70, testy dispatchu zaktualizowane do nowego
  routingu, koherencja i self-konsystencja przy 16 równoległych requestach
  (cztery grupy po cztery identyczne prompty dały po jednym unikalnym wyjściu). SUFIT: mixery są nadal serialne per lane i po tej zmianie
  dominują krok — ich batchowanie to następny lever.
- ✅ **`forge bench` mierzy pełny prefill i produkcyjną ścieżkę GEMM
  (2026-07-25).** Dwa niezależne defekty, oba zawyżały wynik. (1) Auto-fp8
  prefill dla gęstego GGUF włączał się tylko w `serve`; `bench` i `run`
  dostają go teraz też (`ppl` świadomie nie — jest bramką jakości uruchamiającą
  dokładnie ten GEMM, który nazywa `FORGE_GEMM`). Mistral-7B Q4_K_M: 6 332 →
  **14 783 tok/s**. (2) `--prefix-cache` domyślnie `on` powodował, że
  powtórzenia trafiały w cache i przeliczały tylko rozbieżny ogon promptu, a
  czas nadal dzielono przez pełną długość — raportowane 44 537 tok/s wobec
  realnych 14 783. `DrainOutcome` przenosi teraz `cache_read_tokens` do
  benchmarku, trafienie kończy przebieg błędem z dokładną liczbą
  (`cache prefiksów obsłużył 992 z 1024 tokenów promptu`), a domyślną wartością
  w `bench` jest `off`.
- ✅ **Cache prefiksów zweryfikowany — jest poprawny (2026-07-25).**
  Podejrzenie padło na niego, bo `bench` przerywał kontrolą
  `greedy token IDs differ between benchmark repetitions`. Weryfikacja na
  realnym serwerze: to samo żądanie powtórzone 4x na promptcie 1178 tokenów
  daje identyczne wyjście, a prompt dzielący z zapełnionym cache'em tylko
  połowę prefiksu daje identyczny wynik na zimnym i ciepłym serwerze (dwa
  osobne procesy) — radix nie podaje niepasującego prefiksu i nie zanieczyszcza
  KV. Przerwanie w benchu miało inną przyczynę: przebieg zimny prefillował 1024
  tokeny, ciepły tylko 32-tokenowy ogon, a kafel GEMM zależy od liczby tokenów,
  więc logity ostatniego tokena różnią się na ostatnich bitach i przy niemal
  równych logitach z losowego promptu greedy się przewraca. Komentarz w
  `acquire_prefix` twierdził, że pożyczony prefiks jest „bit-identical" — jest
  nim KV, ale nie logity; komentarz poprawiony.
- ✅ **Macierz porównawcza FORGE vs vLLM vs llama.cpp (2026-07-25).** Pełny
  protokół i tabele: `docs/BENCH_MATRIX_2026-07-25.md`. Bielik-7B NVFP4,
  współbieżność FORGE/vLLM: decode-only 158/165 tok/s przy C=1 (0,96x) i
  2 746/4 113 przy C=32 (0,67x) — rozjazd rośnie z batchem i widać go wprost
  w TPOT (vLLM 6,04→7,52 ms od C=1 do C=32, FORGE 6,07→10,93). p1024/o128:
  771/853 tok/s przy C=32 (0,90x), ale mediana TTFT FORGE trzyma 123-206 ms
  w całym zakresie, a vLLM skacze do 630 ms przy C=32 (FORGE 3,1x lepiej) i
  traci przepustowość między C=16 a C=32. Pojedynczy strumień, dokładnie 1024
  tokeny promptu, FORGE/llama.cpp: qwen3-0,6B prefill 0,95x i decode 1,03x;
  Mistral-7B Q4_K_M prefill **1,16x** (14 772 wobec 12 704) i decode 0,94x;
  ThinkingCap-Qwen3.6-27B prefill 0,83x i decode **2,33x** (111,7 wobec 47,9 —
  zysk pochodzi z natywnego MTP, bez spekulacji jest 42,2 czyli 0,88x).
  Bielika llama.cpp nie ładuje (compressed-tensors NVFP4).
  GRANICA METODY: uprząż HTTP `vllm bench serve` liczy własnym tokenizerem
  zarówno prompty, JAK I wyjście, więc nadaje się tylko gdy tokenizer klienta
  jest tokenizerem serwera — prompty „1024-tokenowe" z tokenizera Bielika
  re-tokenizują się do ~2900-3000 pod Mistralem i llama.cpp odrzuciło 223
  żądania. Rzetelna współbieżność z llama.cpp wymaga tokenizerów HF dla GGUF
  albo batchowego bencha w FORGE; nie ma dziś ani jednego, ani drugiego.
- 🟡 **`forge bench` rozjeżdża się z produkcją i z determinizmem (2026-07-25).**
  Auto-fp8 prefill dla gęstego GGUF włącza się wyłącznie w `serve`, więc `bench`
  zaniża prefill Mistrala **2,3x** (6 332 wobec 14 772 tok/s z
  `FORGE_GEMM=fp8mod`) — każdy pomiar prefillu GGUF bez tej zmiennej jest
  nieporównywalny z produkcją. Osobno: `bench` na Mistralu Q4_K przerywa własną
  kontrolą `greedy token IDs differ between benchmark repetitions`, a
  `--prefix-cache off` naprawia powtarzalność. Oba punkty domknięte wpisami
  z 2026-07-25 powyżej.
- ✅ **rmsnorm z residuałem: rozwinięcie odczytów w locie (2026-07-25).**
  Profil pokazał `rmsnorm_residual_f16` na ~138 GB/s i 4,6% czasu GPU: batchowy
  decode startuje jeden blok na token, więc krok B=32 zajmuje 32 z 128 SM,
  a jedynym źródłem równoległości pamięci zostaje liczba odczytów w locie na
  wątek — przy skalarnej pętli f16 były to dwa. Oba przebiegi wykonują teraz
  `NORM_UNROLL = 8` odczytów grid-stride przed konsumpcją; krok w pętli zostaje
  `block_dim.x`, więc wątek odwiedza te same kolumny w tej samej kolejności,
  a strumień residuału jest DOKŁADNY (nowy golden `rmsnorm_residual_matches_reference`
  porównuje go przez `assert_eq!`, pokrywa ścieżkę rozwiniętą i resztkową na
  4096/2048/2568/300 kolumnach; golden 70/70).
  Suma f32 NIE jest bitowo identyczna ze wersją skalarną — rozwinięcie pozwala
  kompilatorowi inaczej kontraktować akumulację. Audyt pełnych logitów wobec
  zapisanej referencji B1 (16 promptów p1024 × 256 kroków) pokazuje dryf
  wewnątrz szumu f16 i lekko na korzyść: zgodność top-1 0,99927 → 0,99951,
  max_abs 1,667 → 1,371, `mean_rel_l2` 0,002533 → 0,002549, kosinus bez zmian
  na sześciu miejscach; sekcja prefill jest bit-w-bit identyczna, a **strumień
  tokenów greedy się nie zmienia** (`free_generation_ids` identyczne, liczba
  divergencji wobec referencji nadal 2). Kolejność sumowania jest więc świadomie
  zostawiona kompilatorowi, a nie przypinana.
  Serve decode-only in32/o256: C=16 1 670→**1 682** tok/s (TPOT 9,02→8,71 ms),
  C=32 2 655→**2 722** (TPOT 11,23→11,00 ms, **+2,5%**).
  Narastająco z kaflem BM32 i dwuetapowym potokiem: C=32 **533 → 2 722 tok/s**,
  TPOT **58 → 11,00 ms**.
- ✅ **BM32: dwuetapowy potok cp.async zamiast trzyetapowego (2026-07-24).**
  Profil pokazał, że projekcje BM32 jadą na 585-661 GB/s przy ~17% occupancy
  (39 936 B shared → 2 bloki/SM). Zejście do dwóch etapów daje 26 624 B →
  3 bloki/SM (12 warpów zamiast 8). Zmiana jest BITOWO neutralna — nie rusza
  kolejności MMA ani redukcji — co bramkuje osobny A/B z kontrolą bit-parity;
  golden BM32 8/8 i test parytetu BM32↔BM16 przechodzą bez zmian metryk.
  Izolowany pomiar RTX 4090 (32 tokeny): gate+up 84,8→64,1 us (**1,32x**),
  qkv 28,7→24,6 (1,17x), o 14,9→13,7 (1,09x), down 34,1→33,3 (1,02x).
  Serve decode-only in32/o256: C=16 1 635→**1 670** tok/s, C=24 1 986→**2 115**,
  C=32 2 493→**2 655** (TPOT 12,01→11,23 ms, **+6,5%** przy C=24 i C=32).
  p1024/o128 bez zmian (658/747 tok/s) — tam limitem jest prefill, nie decode.
  **Wariant ośmiowarpowy (256 wątków, BN64) ZMIERZONY I ODRZUCONY**: 118
  rejestrów × 256 wątków ogranicza do 2 bloków/SM, a szersza redukcja epilogu
  zjada zysk z warpów — gate+up 0,79x, qkv 0,81x, o 0,94x, down 0,96x wobec
  czterech warpów (przy zachowanej zgodności bitowej).
- ✅ **Profil kroku decode: Bielik B=32 i Qwen 27B C=8 (2026-07-24).** Pełny
  raport w `docs/PROFILE_DECODE_2026-07-24.md`. GPU jest zajęte 96% (Bielik)
  i 91% (Qwen) okna pomiarowego, więc narzut launchy i luki między kernelami
  NIE są problemem — liczy się sama treść kerneli. Pułapka narzędziowa:
  bez `nsys --cuda-graph-trace=node` kernele z grafu CUDA nie pojawiają się
  w raporcie i profil wygląda, jakby BM32 w ogóle nie działało.
  Bielik, 10,0 ms kroku decode: projekcje BM32 6,59 ms (585-661 GB/s),
  attention 2,06 ms (776 GB/s), rmsnorm 0,59 ms, głowa logitów F16 0,56 ms
  (465 GB/s, 262 MB na krok — paczka FP8 `lm_head` istnieje, ale używa jej
  tylko prefill), sampling 0,10 ms. Projekcje są wolniejsze od attention na tym
  samym GPU, bo kafel BM32 zjada 39 936 B shared → 2 bloki/SM × 128 wątków =
  ~17% occupancy; podniesienie ich do poziomu attention to ~1,3 ms/krok.
  Qwen 27B: 519 kroków dało ~1040 tokenów, czyli **2 tokeny na krok mimo ośmiu
  aktywnych sekwencji** — `record_hybrid_batch_forward` odrzuca każde `n != 2`,
  więc ścieżka hybrydowa ma architektoniczny sufit B=2. Skalowanie
  współbieżności praktycznie nie istnieje (~40 tok/s przy C=1 → 51 tok/s przy
  C=8), a `gemv_q8_0_dp4a` startuje 385 razy na krok (projekcje DeltaNet
  jednowierszowym GEMV). Strojenie kerneli nie ruszy tego modelu, dopóki
  sufit B=2 stoi.
- ✅ **Kernele BM32 NVFP4-CT: batchowy decode 17..32 (2026-07-24).** Batche
  B=17..32 spadały z wyspecjalizowanych kafli na generyczny dequant-GEMM
  (TPOT 58 ms, 533 tok/s decode-only przy B=32). Moduł
  `kernels/mojo/src/nvfp4_ct_direct.mojo` (dawniej `nvfp4_ct_bm16.mojo`) ma
  teraz drugi kafel fizyczny: `gemm_nvfp4_ct_direct_bm32_bn64_bk128`, BN64/BK128,
  128 wątków, trzy etapy `cp.async` (39 936 B shared, 96-116 rejestrów, zero
  spilli). Jeden dekod wagi w rejestrach zasila dwa kafle M16 przez podwójne
  `ld_matrix` — odczyt wag na token spada o połowę względem dwóch przebiegów
  BM16. Osiem wrapperów `gemm_nvfp4_ct_bm32_{qkv,o,gateup,down}_{m24,m32}`;
  M24 obsługuje wyłącznie ścieżkę z tieringiem KV (bucket schedulera jest
  potęgą dwójki, więc pure decode zawsze trafia w M32).
  Routing w Ruście jest wspólny dla obu kafli: `nvfp4_ct_physical_m` mapuje
  logiczne M4/M8/M16 → BM16 i M24/M32 → BM32, `gemm_nvfp4_ct_padded` dobiera
  kafel N, wątki i głębokość potoku, a `record_batch_forward` liczy offsety
  segmentów QKV/gate-up z fizycznego M, nie ze stałej 16. Bufory segmentowane
  rosną do M32 dopiero od `cap >= 24`.
  Bramki: golden Mojo BM32 8/8 wobec niezależnej referencji
  `gemv_batch_nvfp4_f16_b16` (wszystkie cztery projekcje × M24/M32,
  rel_l2 1,5-1,9e-05, top1 dokładne, ogon fizyczny wyzerowany, canary czyste);
  `forge-kernels` golden 69/69 i `sampling` 8/8; nowy test
  `nvfp4_bm32_zgadza_sie_z_bm16_i_obiema_polowkami_kafla` (32 kroki greedy na
  16 realnych promptach p1024) — obie połówki kafla identyczne i zgodne z
  audytowanym BM16; koherencja serve przy 32 równoległych requestach.
  RTX 4090, wolne GPU, ten sam checkpoint Bielik NVFP4, ten sam klient
  (`vllm bench serve`, obraz 0.25.1), `forge serve --max-active 32 --ctx 2048
  --kv-pages 1536`:

  | Scenariusz | C | FORGE przed | FORGE po | vLLM |
  |---|--:|--:|--:|--:|
  | decode-only in32/o256 (tok/s) | 16 | 1 670 | 1 635 | 2 332 |
  | decode-only in32/o256 (tok/s) | 24 | — | 1 986 | 3 213 |
  | decode-only in32/o256 (tok/s) | 32 | 533 | **2 493** | 4 139 |
  | p1024/o128 (tok/s) | 16 | 656 | 659 | 756 |
  | p1024/o128 (tok/s) | 24 | — | 666 | 813 |
  | p1024/o128 (tok/s) | 32 | — | 746 | 853 |

  TPOT median B=32 spadł z **58 ms do 12,01 ms** (decode-only), czyli 4,7×.
  Mediana TTFT p1024 pozostaje mocną stroną FORGE: 208/198/208 ms wobec
  577/617/633 ms vLLM przy C=16/24/32. Cel „przegonić vLLM we
  współbieżności" NIE jest osiągnięty: na czystym decode vLLM ma 1,66×
  (C=32), na p1024 1,14×. Uwaga metodologiczna: wcześniejsze liczby vLLM
  (906 tok/s p1024 C=16) pochodzą z sesji, w której na GPU rezydowała
  instancja tentaflow i `--gpu-memory-utilization 0.72`; pomiar z 2026-07-24
  jest jedynym, w którym oba silniki mierzono tego samego dnia na wolnym GPU.
  Wykonawczo sprawdzono wyłącznie CUDA/RTX 4090; źródła zachowują przenośny
  fallback, ale AMD i Metal nie były uruchamiane.
- 🟡 **Katalog kerneli rozjechał się z manifestem — pełny build regresuje
  (znalezione 2026-07-24).** `scripts/build_kernel_catalog.py` nie parsował
  `dump_asm=Path(` złamanego na dwie linie i wywracał się na
  `deltanet_commit_recompute_segmented_shared_d128_f32`, więc `pixi run mojo
  build_kernels.mojo` ORAZ `test_build_kernel_catalog.py` failowały na czystym
  HEAD. Parser jest naprawiony i pokazuje teraz prawdziwy problem: 14 kerneli
  jest w `manifest.json`, ale NIE w katalogu (`topk_batched_{partial,final}_f32`,
  `gemv_q{4,6}_k_dp4a_batch_b{2,4,8,16}`, `pack_q{4_k,6_k,8_0}_fp8`,
  `gemm_q8_0_i8mma_b16` — dodane wczoraj wyłącznie izolowanymi builderami),
  a 13 jest w katalogu, ale nie w manifeście (`gemm_q6k_i8_native_*` po
  rewercie i stary `topk_batched_f32`). Skutek: pełny build katalogu USUNIE
  dwuprzebiegowy sampler i small-batch GEMV Q4_K/Q6_K, a przywróci zrewertowany
  Q6_K native. Do czasu uzgodnienia obu list nowe kernele należy publikować
  izolowanym builderem w wariancie z manifestem (5-argumentowe
  `validate_sm80_ptx.py`), tak jak `build_nvfp4_ct_bm32.mojo`.
- ✅ **Deficyt puli wag dla paczek FP8 jest zgłaszany wprost (2026-07-24).**
  `build_fp8_ffn` i `build_fp8_modular_auto` zwracają `Fp8PackOutcome`
  (`Built` / `Unsupported` / `PoolShortfall { required, available }`) zamiast
  `bool`, więc brak wsparcia sprzętu przestał być nieodróżnialny od zbyt małej
  puli. `serve` loguje `WARN` z polami strukturalnymi i drukuje operatorowi
  konkretną receptę: `KvPoolProbe` przeszukuje `kv_pool_bytes`, żeby podać
  liczbę stron KV, których zwolnienie REALNIE pokrywa deficyt (średnia na
  stronę zaniża wynik, bo slab każdej warstwy jest zaokrąglany osobno).
  Zmierzone na pułapce z 2026-07-25: `--kv-pages 2048` daje
  „wymagają 6,53 GiB, pula ma 6,16 GiB (brakuje 0,37 GiB) — zmniejsz
  --kv-pages o co najmniej 80"; `--kv-pages 1968` buduje paczki za pierwszym
  razem. Automatyczne przycinanie `--kv-pages` świadomie NIE zostało
  zrobione: jawna wartość operatora jest wiążąca, a ciche zmniejszenie
  pojemności KV zamieniłoby jedną niespodziankę na drugą.
- ✅ **Auto C1 T128 dla hybrydowego NVFP4 (2026-07-22).** Brak
  `FORGE_HYBRID_PREFILL_CHUNK` wybiera chunk 128 wyłącznie dla gęstego qwen35
  z `d_state=128`, FFN NVFP4 GGUF, kompletem artefaktów T128 i NVIDIA warp32.
  Checked estimator wymaga miejsca na jednoczesny scratch zwykłego prefill,
  verifiera cap=4 i prefill cap=128 oraz 64 MiB rezerwy puli activations;
  obowiązkowy `HybridBufs` powstaje przed sprawdzeniem budżetu. Po wyborze T128
  wszystkie bufory cap4/cap128 i oba zestawy potrójnego stagingu pinned-host
  są alokowane przed zakończeniem startupu, co usuwa okno między selekcją a
  pierwszym prefillem. Gdy pełny gate T128 nie mieści się w budżecie, Auto
  wybiera największy istniejący wariant zgodny z backendem, artefaktami i pulą;
  na zweryfikowanym NVIDIA jest to co najmniej T32, a backend przenośny wybiera
  T2/T3/T4/T8/T16 zgodnie z warpem i `max_threads_per_block`. Jawny zakres 3..=1024 nadal nadpisuje Auto do limitu
  danego backendu; extended przechodzi pełny gate także dla jawnej wartości,
  a niemożliwa konfiguracja kończy startup błędem `Unsupported`. Literalne
  `auto` jest równoważne brakowi zmiennej.
  Wybór jest niezmienny w instancji modelu, a profil, alokacja i wykonanie
  korzystają z tej samej wartości. Bufory sprawdzają zapisaną pojemność przed
  ponownym użyciem. Nie ma wykonawczej deklaracji wydajności AMD ani Metal.

- ✅ **Kary samplingu OpenAI na CPU i GPU Mojo (2026-07-21).**
  `frequency_penalty`, `presence_penalty`, `repetition_penalty` i okno
  `repeat_last_n` działają w API, samplerze CPU oraz pojedynczej i batchowej
  ścieżce GPU. Historia obejmuje prompt i odpowiedź. Mojo nakłada wszystkie
  kary z histogramu jednym kernelem przed istniejącym równoległym argmax/top-k,
  bez D2H histogramu w trybie release. `logprobs` są liczone po karach.
  Greedy bez kar zachowuje niezmieniony fast path. Mikrobenchmark RTX 4090 dla
  słownika 151936: greedy 6,99 us, top-k 20 z karą 53,73 us, top-k 64 z karą
  160,05 us.

- 🟡 **Hybrydowy prefill NVFP4 B2 T32 w Mojo (2026-07-22).** Scheduler może
  połączyć dokładnie dwa requesty i wykonać segmenty `B=2`, `T=32`; funkcja
  domyślnie działa w trybie auto tylko przy pełnym capability modelu na
  NVIDIA warp32. `FORGE_HYBRID_PREFILL_BATCH=0` wymusza B1 bez alokacji
  dedykowanego scratchu, a `1` rygorystycznie wymaga kompletnego capability
  i artefaktów. Jawne włączenie na NVIDIA warp64, AMD,
  Apple albo CPU zwraca `Unsupported` przed routingiem do PTX. Źródła Mojo są
  przygotowane do dalszego portowania, lecz AMD i Metal nie zostały wykonawczo
  zweryfikowane. Target B2 zachowuje pełne ID i bitowy parytet z dwoma
  seryjnymi lane; rollback i awaria rollbacku obejmują całą parę, a ta druga
  zatruwa i poddaje kwarantannie oba stany MTP oraz wspólny cache. Catch-up MTP
  nadal ma dwa seryjne przebiegi macierzowe lane po lane. Scratch wynosi
  450 692 688 B (429,81 MiB). Profil samplowania GPU dla dwóch requestów po
  osiem tokenów zawierał 18 150 launchy, osiem synchronizacji oraz osiem
  transferów D2H po 8 B, bez transferu logitów całego słownika.
  Gauge `forge_engine_hybrid_prefill_b2_scratch_bytes` pozostaje zerowy w trybie
  off i raportuje faktyczny logiczny rozmiar po pierwszym udanym B2.
  Poniższe historyczne A/B używało ówczesnego domyślnego C1 T32; po włączeniu
  Auto C1 T128 nie jest aktualnym porównaniem B2 do domyślnego C1.
  Pięć końcowych prób raw128 dało medianę Auto/OFF **309,7/248,6 tok/s**, TTFT
  **826,69/1029,87 ms** i E2E **1119,60/1322,60 ms**. Raw512 ON dało
  **320,5/320,2/319,9/320,0/320,2 tok/s** z medianą **320,2 tok/s**, TTFT
  **3198,02 ms** i E2E **3505,75 ms**; odniesienie OFF wyniosło **251,4
  tok/s**, około **4073 ms TTFT** i **4380 ms E2E**. Artefakty `/tmp` nie są
  wersjonowane.

- 🟡 **Natywne Qwen3.5/3.6 MTP/NextN dla gęstego NVFP4 GGUF
  (2026-07-22).** Rejestr `qwen35` oddziela blok `nextn_predict_layers` od
  64-warstwowego trunku `protoLabsAI/ThinkingCap-Qwen3.6-27B-MTP-GGUF` bez
  drugiej kopii targetu. Proposer K=2/K=3, verifier greedy, argmax, KV i DeltaNet
  działają na GPU przez kernele Mojo. Scheduler paruje dwa requesty o tym samym
  K w natywnym B2. Segmentowane KV, attention, DeltaNet, decyzje i commit
  zachowują stan per lane; commit odtwarza zaakceptowany stan ze wspólnego
  forwardu bez puli retained checkpointów. Błąd checkpoint/rollback zatruwa i
  poddaje kwarantannie oba lease'y pary. Pięć powtórzeń RTX 4090 dało medianę
  B2 ON/OFF: raw128 **137,40/101,97 tok/s** (+34,75%), raw512
  **97,78/76,38 tok/s** (+28,02%); stałe K=3 osiągnęło **136,97/94,34
  tok/s**. Wszystkie przebiegi zachowały pełne ID względem serial greedy.
  Szybsze pomiary llama.cpp B2 są nieważne porównawczo: tylko 5/24 wyjść
  zgadzało się z oracle `np1`. Różne K, tiering, niepełna para i
  niespełniony kontrakt capability przechodzą na B1;
  `FORGE_NATIVE_MTP_B2=0|1` jest ścisłym kill-switchem. vLLM 0.25.1 nie
  dostarczył porównywalnego wyniku dla lokalnego jednoplikowego GGUF. Draft ID,
  pack `[B,T]` i gather embeddingu F16/Q8_0/NVFP4 działają na GPU, a cykl B2 ma
  jeden końcowy D2H i sync. Profil 24 cykli potwierdził dokładnie jeden sync i
  cztery małe H2D na cykl. Gather zeruje błędne ID bez GPU OOB, a finalna
  walidacja zwraca kontrolowany błąd. Współczesne A/B względem `7d472a0a` dało
  +0,56% dla raw128 i +0,12% dla raw512. Checkpoint shared-Q8 współdzieli jedną
  kwantyzację `pb.x` wyłącznie między `gate_proj`, `alpha_proj` i `beta_proj`;
  `out_proj` nadal osobno kwantyzuje `normed`. Izolowany mikrobenchmark RTX 4090
  dla wierszy `[5120, 48, 48]` i 5120 kolumn skrócił T6 z 53,452 do 47,691 us
  (+10,78%), a T8 z 58,537 do 54,451 us (+6,98%), redukując grupę z 6 do 4
  uruchomień. Dla 48 warstw DeltaNet oczekiwane jest 192 -> 96 wywołań
  `quantize_act_q8_1` na cykl B2. Pełny profil raw512 zawiera 224 kwantyzacje
  na cykl: 96 dla tej grupy DeltaNet i 128 dla pozostałych projekcji targetu
  oraz MTP.
  Testy exact/canary/top1, multistream T6 -> T8 z realokacją scratchu oraz
  testy błędów eventu przeszły. Realny 27B shared-Q8 zachował pełne ID; pięć
  powtórzeń osiągnęło medianę 132,33 tok/s dla raw128 i 100,74 tok/s dla
  raw512. Profil `nsys` potwierdził 16 warstw attention i 224 kwantyzacje na
  cykl B2.
  Rollout N/N B2 paruje dwa pełne drafty n-gram o tym samym K=2 lub K=3.
  Brak `FORGE_MTP_NGRAM_BATCH` wybiera `auto` dla modelu z capability na
  zweryfikowanym NVIDIA warp32, `0` wymusza B1, a `1` pozwala na eksperymentalny
  backend przy zachowaniu wymagań strukturalnych modelu. AMD/Metal w `auto`
  pozostają w B1. Segmentowany KV-only catch-up używa kerneli
  Mojo `mtp_norm_join_shifted_segmented_f16`,
  `kv_append_batch_segmented_masked_f16` i
  `mtp_commit_catchup_metadata_segmented`. NVIDIA używa dokładnej segmentowanej
  attention z czterema warpami, zgodnej bitowo z verifierem seryjnym. Golden
  obejmuje T1/ctx1, T6/ctx31/32/33/128, T8/ctx512/2048, zamianę lane, różne
  mapy stron i canary;
  memcheck zakończył się bez błędów. Realna macierz retained 1..T dla K2/K3,
  lane swap oraz cancel/reuse przeszły. Pięć powtórzeń N/N ON/OFF zachowało
  identyczne pełne ID: raw128 159,70/122,87 tok/s (+29,98%), raw512
  94,33/83,78 tok/s (+12,59%). Dedykowany licznik potwierdził odpowiednio 32
  i 20 cykli N/N na przebieg oraz zero przy wyłączonej fladze. `nsys`
  potwierdził po jednym norm/join, maskowanym append KV, commicie metadanych i
  końcowej synchronizacji na cykl N/N. Prometheus eksportuje dedykowany licznik
  `forge_engine_mtp_ngram_b2_steps_total`. Smoke raw128 potwierdził liczniki
  `auto=32`, `0=0`, `1=32` i identyczny pełny hash ID; realne lane-swap oraz
  cancel/reuse/izolacja przeszły. Mieszane N/M, M/N i M/M używa tego samego
  verifiera po jawnym ustawieniu `FORGE_MTP_NGRAM_MIXED_BATCH=1`; brak zmiennej
  pozostawia auto wyłącznie dla N/N. Macierz raw128 dała około +3,6% względem
  N/N-only i +5,6% względem B1, ale raw512 była około 0,1% wolniejsza od
  N/N-only. Te liczby poprzedzają osobny oracle B1 per lane i są wyłącznie
  pomiarem przepustowości. Direct model/source-mask, syntetyczna macierz i
  memcheck mixed przeszły; realny server parity per lane oraz pełny `nsys`
  oczekują na co najmniej 22,5 GiB wolnego VRAM. Auto rollout mixed pozostaje
  wyłączony.
  CUDA jest jedynym backendem sprawdzonym wykonawczo;
  źródła zachowują przenośny fallback dla AMD/Metal. Raport:
  `docs/BENCH_QWEN35_MTP_NVFP4.md`.

- 🟡 **NVFP4 Bielik: hybrydowy prefill FP8 i GQA decode w Mojo
  (2026-07-20).** Opcjonalne `FORGE_GEMM=fp8mod-ffn` przepakowuje na GPU
  projekcje Q/O/gate/up/down z rezydentnych wag NVFP4 do FP8, a `lm_head` z F16
  do FP8 dla pojedynczego strumienia; K/V i źródłowe wagi decode pozostają NVFP4.
  Pakowanie całego modelu trwa około 49 ms i jest wykonywane raz przy ładowaniu.
  Małe batche mają wyspecjalizowane Mojo GEMV B4/B8/B16 oraz GEMM BM32. Decode
  modelu GQA 4:1, `head_dim=128`, KV F16 i bez Q/K norm współdzieli odczyt K/V
  dla czterech głowic Q, a `combine2` łączy dwie głowice na CTA. Routing sprawdza
  możliwości urządzenia, artefakty, kształty i dostępny VRAM przed alokacją;
  w przeciwnym razie atomowo pozostaje na NVFP4. Golden GPU **49/49**, kanoniczny
  prompt PASS, jakość `NLL=1,15248`, `PPL=3,1660` wobec baseline
  `1,15260`/`3,1664`. Końcowy pomiar RTX 4090 pp4096: FORGE
  **10 302,7 tok/s** wobec vLLM 0.25.1 **9 732,9** (**+5,85%**); decode FORGE
  **143,100** wobec **146,372 tok/s** (**-2,24%**, cel nadal niespełniony).
  Większe splity attention, BK64, RPB24 i prototyp Marlin były NO-GO po pomiarze.
  `llama.cpp` nie obsługuje bezpośrednio badanego checkpointu compressed-tensors
  NVFP4. HAL nadal ma tylko CUDA: brak AMD/ROCm i Metal; natywne FP4 Blackwell
  także nie jest zaimplementowane. Pełny protokół: `docs/BENCH_NVFP4_VLLM.md`.

- ✅ **Split flash-decode: efektywne splity zależne od kontekstu (2026-07-25).**
  `attn_decode_split` płacił prolog (redukcje q/k-norm, RoPE, append) w KAŻDYM
  z 8 splitów per (seq, głowa) — przy ctx ~200 (grid 16×32×8 = 4096 bloków ×
  ~1,5 µs prologu w falach) attention kosztowała 2,25 ms z 8,9 ms kroku B16.
  Teraz `eff_splits = clamp(ceil(ctx/256), 1, n_splits)`; nadmiarowe bloki
  publikują neutralny partial (m=-inf, l=0 — zero wagi w combine) i wychodzą
  przed prologiem; przy ctx >= 256·n_splits chunking bitowo identyczny ze
  stałym splitem. Pomiar Bielik decode-only C=16: **1 512 → 1 670 tok/s**
  (TPOT 9,83 → 9,11 ms). ZMIERZONE PUŁAPKI infrastruktury: (1) --kv-pages
  2048 przy auto-puli wag wypycha paczki FP8 poza preflight (7,0 GB vs 6,6) i
  cicho gasi szybki prefill — C=32 wymaga kv-pages ~1536; (2) decode B=17..32
  spada z kerneli bm16/B16 na generyczny dequant-GEMM: TPOT 58 ms przy B=32
  (533 tok/s decode-only). Obie pułapki są domknięte wpisami z 2026-07-24:
  kernele BM32 dla NVFP4-CT oraz jawny raport deficytu puli wag.
- ✅ **Batchowy sampler top-k: O(k²·V) → dwuprzebiegowy (2026-07-25).**
  Profil perf+nsys serve pod obciążeniem pokazał, że `topk_batched_f32`
  zjadał **10,08 ms na krok** (98,8% czasu GPU przy qwen3-0.6b: k=40 rund ×
  skan 152k słownika × O(k) porównań id per element, jeden blok na
  sekwencję) — to było całe rzekome „10 ms narzutu hosta"; emisja/tokio były
  bezczynne. Krok 1: maskowanie in-place zamiast porównań id (O(k·V), 10→3,1
  ms). Krok 2: pełna wymiana na dwuprzebiegową strukturę szybkiej ścieżki
  single-row — `topk_batched_partial_f32` (grid chunks×seqs, slice w shared,
  lokalne top-k) + `topk_batched_final_f32` (merge per sekwencja + replay
  softmax/min-p/top-p/hash z per-seq parametrami); scratch parts grow-only
  poza grafami. Stary kernel usunięty. Parity `gpu_sampling` 5/5, golden
  69/69. Serve decode-only (in32/o256): qwen3-0.6b C=8 **560→2 496 tok/s**
  (TPOT 12,65→2,72 ms), C=16 886→2 248; Bielik-7B C=16 **1 291→1 512**
  (TPOT 9,83). p1024/o128: qwen C=8 501→**1 577** (TTFT 80 ms), C=16
  719→**1 453**; Bielik C=8 448→**523**, C=16 612→**656** (vLLM 906 → luka
  1,38×). Host-side na krytycznej ścieżce zostało ≤0,4 ms — planowany
  „async pipeline" emisji okazał się zbędny (dane, nie przypuszczenia).
- ✅ **Mixed prefill+decode forward — WYKONANY (2026-07-24/25).** Tokeny
  decode (B sekwencji GPU-sampled, spec off) jadą jako dodatkowe wiersze w
  forwardzie chunka prefillu: `prefill_forward_lanes` dostał
  `MixedDecodeRows` (wiersze decode ZA chunkiem, surowe przez qk-norm/rope
  chunka — `attn_decode_split` sam robi norm+RoPE+append+attention po
  metadanych z batch bufs), `Model::mixed_prefill_decode_step` (wzrost KV,
  pinned upload metadanych, wspólna głowa logitów B(+finalny wiersz chunka),
  batchowy sampling), scheduler: `mixed_gpu_group` zamiast pary
  (batched decode + FIFO chunk); `FORGE_MIXED_STEP=0` = kill-switch.
  Wiersze decode WLICZAJĄ się do kwantu (take = quantum − B — 1024+16 wpadało
  w dodatkowy kafel GEMM, −12%). Gate'y: capable = dense, non-hybrid/moe/rot,
  bez tier/kalibracji, głowa F16/Q8_0/Q4_K/Q6_K (NvFp4Gguf ma tylko kernele
  potęg dwójki). Pomiary p1024/o128: Mistral C=16 out **245→281 tok/s
  (+15%)**, TTFT med 918→713 ms, TPOT 54,5→47,2; C=8 TTFT 221→170. Bielik
  TTFT med C=8 209→180, C=16 224→197 przy neutralnym out (−2..4%, w szumie —
  jego baseline decode był już graphowany i tani). Greedy parity: teksty
  identyczne z trybem off; engagement potwierdzony trace'em. Dalsze możliwe:
  graf części decode w mixed passie, polityka per model.
- 🟡 *(zrealizowane wyżej)* **Mixed prefill+decode forward — zmierzony headroom i plan (2026-07-24).**
  Pomiar rozstrzygający: czysty decode B=16 Bielika (in32/out256) daje
  **12,2 ms TPOT / 1253 tok/s** — poziom vLLM; cała pozostała luka przy p1024
  (628 vs 906 tok/s, TPOT 22,4 vs 12) to interferencja chunków prefillu.
  Skan chunka przy C=16: 256 → 499 tok/s, 512 → 606, **1024 → 628** (mniejsze
  chunki wydłużają łączny czas prefillu bardziej niż zyskują na częstszych
  krokach decode); default serve podbity do 1024.
  Plan mixed forward (dense, non-hybrid, GPU-sampled): jeden forward nad
  [B_decode + T_chunk] tokenach — wiersze decode doklejone PRZED wierszami
  chunka do buforów prefillu; wspólne norm/GEMM/SwiGLU po całym T_total;
  rope dwoma launchami (pozycje per-seq dla wierszy decode, sekwencyjne dla
  chunka); attention rozdzielone: `attn_decode_f16` (n_seqs=B) na wierszach
  0..B i prefill-attention na B..B+T; KV append per-row (decode: po 1 tokenie
  do własnych sekwencji, chunk: T do swojej); logity B wierszy decode + (final
  chunk) ostatni wiersz chunka przez `logits_gemm` (per-lane fallback już
  obsługuje Q4_K/Q6_K); sampling `sample_batched_argmax/topk`. Scheduler:
  gdy jest żywy decode i pending prefill → `mixed_step` zamiast pary
  (batch_gpu_decode + advance). Grafy: krok mieszany bez grafu (chunk jest
  compute-bound, narzut launchy amortyzowany), czysty decode zachowuje
  dotychczasowe grafy bucketowe. Oczekiwany efekt: TPOT przy prefill w locie
  ~12-14 ms, out C=16 ~850-900 tok/s.
- ✅ **Batched dense prefill dla głów Q4_K/Q6_K + polityka cold-burst
  (2026-07-24).** `DensePrefillLogitsKind` dostał warianty Q4K/Q6K (głowa
  batch prefillu idzie przez istniejący per-lane dp4a sweep w `logits_gemm`),
  więc Mistral Q4_K_M łapie się na grupowy prefill B4/B8/B16. Pomiar pokazał
  jednak, że grupowanie przy ŻYWYM decode psuje medianę TTFT (grupa kończy
  się razem: C=8 med 416 vs 182 ms FIFO) przy +4% out — dlatego grupowy
  prefill działa teraz TYLKO gdy nic nie dekoduje (zimny burst), a przy żywym
  decode rządzi FIFO. Wynik (p1024/o128): Bielik C=16 **606 tok/s / TTFT med
  216 ms** (przed dniem zmian: 121 / 4053; vLLM: 906 / 105), C=8 465 / 209;
  Mistral C=8 298 / 221. Ostatni duży lever: prawdziwy mixed
  prefill+decode forward (tokeny decode w GEMM-ach chunka prefillu).
- ✅ **Scheduler: FIFO serial prefill — jedna sekwencja na iterację
  (2026-07-24).** Pętla schedulera prefillowała KAŻDĄ oczekującą sekwencję
  pełnym kwantem w jednej iteracji, więc burst nowych promptów wstrzymywał
  decode na N×chunk (C=16: ~16×50 ms między krokami decode) i TTFT wszystkich
  promptów degenerował do ogona bursta. Teraz serial prefill przesuwa najwyżej
  JEDNĄ (najstarszą — `active` trzyma porządek przyjęć) sekwencję na iterację;
  batched dense prefill dalej zbiera całe grupy 4/8/16, decode dostaje krok co
  najwyżej jeden chunk. Pomiar p1024/o128: Bielik NVFP4 C=16 TTFT med
  **937→330 ms** (mean 1012→428), out **543→594 tok/s**; C=8 TTFT med 276→209.
  Mistral Q4_K C=8 TTFT med 320→182 ms (out bez zmian). Ograniczenia dalej:
  batched dense prefill wymaga lm_head F16/Q8_0/NvFp4Gguf (Q6_K Mistrala nie
  łapie się — kandydat: per-lane głowa jak w batched decode) i dokładnych grup
  4/8/16; pełny mixed prefill+decode forward (tokeny decode doklejone do GEMM
  chunka) pozostaje otwarty.
- ✅ **GPU pack GGUF→e4m3: paczki fp8 w 0,1 s (2026-07-24).** Kernele Mojo
  `pack_q{4,6}_k_fp8` i `pack_q8_0_fp8` (`src/pack_gguf_fp8.mojo`, builder
  `build_pack_gguf_fp8.mojo`, 36-40 rej., 0 spill, `.target sm_80`) pakują
  REZYDENTNE surowe bajty GGUF do e4m3: jeden blok 256 wątków na wiersz,
  pass 1 absmax po dequancie on-the-fly, pass 2 kody `x*448/absmax`. Dequant i
  konwersja e4m3 są repliką forge-formats GAŁĄŹ PO GAŁĘZI — golden
  `pack_gguf_fp8_matches_cpu_pack` wymaga BITOWEJ równości kodów i skal z
  packiem CPU dla wszystkich trzech formatów (69/69). `Model::build_fp8_gpu`
  obsługuje fused QKV/QK/gate-up przez okna wierszy (prewalidacja formatów
  przed jakąkolwiek alokacją); ścieżki auto (serve) i `FORGE_GEMM=fp8/fp8mod`
  próbują GPU najpierw, CPU rebuild (7 wątków/warstwę) zostaje fallbackiem dla
  innych formatów. Efekt: budowa paczek Mistrala-7B **117 s → 39,5 s (CPU
  równoległy) → 0,1 s (GPU)**; serve startuje w 3,5 s. `ppl backend=fp8mod`
  = **30,5211** — identyczne z dotychczasową ścieżką CPU; koherencja i C=8
  bez zmian (301 tok/s / TTFT 320 ms).
- ✅ **Q8_0 small-batch decode + równoległy pack fp8 (2026-07-24).**
  (1) Batched decode Q8_0 routuje T=2/4/8/16 na istniejące weight-stationary
  `gemm_q8_0_i8mma_b*` (nowa instancja b16, 95 rej., 0 spill; launcher
  `gemm_q8_0_small_batch_at` dzieli scratch `qk_batch` z dp4a);
  `small_batch_decode_capable` obejmuje Q8_0 → auto batch-min=2. Pomiar
  `batched_decode::throughput_scaling` (qwen3-0.6b-q8_0, RTX 4090): agregat
  B=4 **110→2031 tok/s**, B=8 **3131**, B=16 **2730** (B=1/32 bez zmian).
  Wariant T=16 jako dwie połówki b8 ZMIERZONY i odrzucony (2510 vs 2730 —
  drugi przebieg wag kosztuje więcej niż ulga rejestrowa); monolityczny b16
  zostaje, dalsze strojenie = praca kernelowa.
  (2) `rebuild_fp8` liczy dequant+kody e4m3 siedmioma wątkami na warstwę
  (upload na wątku głównym, peak RAM = jedna warstwa f32): budowa paczek dla
  Mistrala-7B **117 s → 39,5 s**. Pełne zejście <1 s wymaga packa na GPU
  (dequant Q4_K/Q6_K→e4m3 kernelem, jak `pack_nvfp4_fp8` w fp8mod-ffn) —
  otwarte.
- ✅ **Small-batch dp4a GEMV dla Q4_K/Q6_K batched decode (2026-07-24).**
  Nowe kernele Mojo `gemv_q{4,6}_k_dp4a_batch_b{2,4,8,16}`
  (`src/decode_dp4a_batch.mojo`, builder `build_decode_dp4a_batch.mojo`,
  PTX `.target sm_80`, 0 spill, ≤95 rej.): jeden przebieg wag obsługuje
  wszystkie tokeny batcha; matematyka per wiersz identyczna z
  `_dot_q4k_i8`/`_dot_q6k_i8`, aktywacje z `quantize_act_q8_1` ([T,K] int8 +
  block-major skale/sumy). Launcher `gemm_qk_dp4a_batch_at` ma DEDYKOWANY
  scratch alokowany od razu na pułap T=16 (stabilne adresy dla przechwyconych
  grafów decode; wspólny `prepare_q8_1` odpada — jego eventy łamią capture:
  `CUDA_ERROR_CAPTURED_EVENT`). Routing w `gemm_rows` dla T=2/4/8/16 z
  fallbackiem na dotychczasowe GEMM; `small_batch_decode_capable` obejmuje
  teraz Q4_K/Q6_K, więc auto batch-min=2. Golden agregatowe relL2 <5e-3 dla
  obu formatów i wszystkich T (68/68). Serve Mistral-7B Q4_K_M p1024/o128
  (GPU bez rezydentnej instancji): C=4 **133→263 tok/s** (TPOT 27,7→12,8 ms),
  C=8 **136→305** (56,4→22,3), C=16 **212→248** (60,8→50,9 — b16 do
  strojenia, kandydaci: ROWS_PER_BLOCK=8, podział tokenów na 2 CTA). Wyjścia
  równoległe bit-stabilne z serialem („Paris"), ppl default 30,0702 i bench
  single-stream bez zmian.
- ✅ **Serve: współbieżność i TTFT out-of-box (2026-07-24).** Cztery zmiany
  zmierzone klientem `vllm bench serve` (random p1024/o128, RTX 4090 obok
  rezydentnej instancji ~3,6 GiB):
  (1) `--prefill-chunk` domyślnie 512 (było 16 — TTFT C=1 spadał z 2351 ms do
  ~130 ms); `forge run` używa pełnego chunka (`MAX_PREFILL_CHUNK`).
  (2) `--batch-min` domyślnie AUTO per model: 2 gdy wszystkie projekcje mają
  kernele small-batch decode (`ModelWeights::small_batch_decode_capable`,
  NVFP4/NvFp4Gguf), inaczej 12 (formaty token-tile GEMM amortyzują płaski kafel
  dopiero przy ~12 sekwencjach; Mistral Q4_K C=4: batched 46 ms vs 26 ms
  serialized). Silnik: `spawn_engine_batched(batch_min=0)` = auto,
  `FORGE_BATCH_MIN` nadal nadpisuje.
  (3) `--weights-pool-gb` serve domyślnie 0 = auto-split wolnego VRAM
  (`load_for_serve` wcześniej clampował 0 do 1 GiB); dzięki temu auto-FP8
  hybrydowy prefill NVFP4 przechodzi preflight bez ręcznej puli.
  (4) Batchowa głowa logitów: Q4_K/Q6_K `lm_head` przez per-lane dp4a GEMV
  (gemv_*_out_f32 dostały offsety y/x per lane) — wcześniej batched decode
  modeli z głową Q6_K (Mistral) ubijał requesty błędem `Unsupported`.
  Auto fp8mod dla GGUF (serve only): FORGE_GEMM nieustawiony + gęsty GGUF +
  urządzenie fp8_native + komplet instancji `gemm_fp8_mod_{N}_{K}` + preflight
  puli → `Model::build_fp8_modular_auto` buduje paczki e4m3 (~117 s dla
  Mistrala 7B) i prefill idzie przez Modular fp8 (jakość +0,69% PPL wg Finding
  F/I; decode zostaje natywny). Bench/ppl/run nietknięte (auto tylko w serve).
  Efekt (Bielik NVFP4, C=1/4/8/16, gołe flagi): TTFT med 142/233/276/937 ms,
  out 123/270/458/543 tok/s — z 39→543 przy C=16 względem starych defaultów;
  vLLM 0.25.1 na tym samym checkpoincie: 128/417/672/906 tok/s (luka 1,45-1,67×
  w decode przy współbieżności — brakujący lever: small-batch GEMV dla Q4_K/Q6_K
  i CUDA-graphed batched step). Mistral serve C=1: TTFT 132 ms (auto fp8),
  decode bez zmian. Testy: golden 67/67, koherencja 4×parallel „Paris", ppl
  default 30,0702 bez zmian.
- ✅ **REWERT native Q6_K int8 prefill GEMM (2026-07-24).** Zgodnie z werdyktem
  poniżej (revert candidate) routing `gemm_q6_k_f16_at` wraca bezwarunkowo na
  przenośny f16 `gemm_q6_k_f16`; usunięte: launcher `gemm_q6k_i8_native`,
  12 wpisów registry + PTX + manifest, `gemm_q6k_i8_multistage.mojo`,
  `modular_i8/multistage_i8_q6k_native.mojo`, `build_q6k_native.mojo` i golden
  native. Pomiar po rewercie (RTX 4090, obok rezydentnej instancji ~3,6 GiB
  VRAM): Mistral Q4_K_M pp4096 **3 927 → 5 061 tok/s (+29%)**, decode 140,3 bez
  zmian, `forge ppl` 30,0702 (ścieżka f16 bez q8_1 na Q6_K; baseline MMQ 30,31),
  koherencja „Paris, France" OK, golden 67/67. Dług dalszego domknięcia luki do
  ~11k MMQ (fuzja rmsnorm→q8_1, szybki Q6_K Mojo, shared activation) pozostaje.
- 🟡 *(zrewertowane 2026-07-24, patrz wyżej)* **NATIVE-LAYOUT Mojo int8 Q6_K
  prefill GEMM (down-proj + attn_v) — ODZYSKANIE REGRESJI PREFILL (2026-07-20).** Po wycofaniu CUDA MMQ (100 % Mojo)
  prefill Q4_K spadł ~2× (5742 tok/s = 0.51× dawnego MMQ 11151), bo Q6_K
  down-proj przeszedł na WOLNY f16 `gemm_q6_k_impl` (19 % pp4096 w nsys). Fork
  Q4_K native → `modular_i8/multistage_i8_q6k_native.mojo` + wrapper
  `gemm_q6k_i8_multistage.mojo`: czyta surowe bajty `block_q6_K` (210 B/256 wag,
  ql+qh 6-bit rozpakowane w kernelu do int8 `q6−32`, true 1× VRAM). Ziarnistość
  skali Q6_K to 16 (nie 32), więc jedno m16n8k32 mma obejmuje DWA sub-bloki skali:
  flush robi PODWÓJNE mma na 32-region (pełne = S_lo+S_hi, drugie z wyzerowaną
  górną połówką k = S_lo, S_hi = S_full−S_lo), potem
  `acc += da·(dsc_lo·S_lo + dsc_hi·S_hi)` — bez members min (offset −32 w kodzie
  wagi). Aktywacja dzieli q8_1 quant + scratch z Q4_K native. Routing w
  `gemm_q6_k_f16_at` (native najpierw, fallback f16). Bit-exact vs CPU Q6_K×q8_1
  (test `gemm_q6_k_native_prefill_matches_formats_dequant`, relL2 < 5e-3). **Efekt:
  prefill pp4096 5742→4467 tok/s (maintainer re-measured warm-stable; the agent's "11154/1.94×" was FALSE — a bad measurement. The native Q6_K double-mma is actually SLOWER than the f16 it replaced, so this REGRESSED prefill 5742→4467 = 0.40× the old CUDA MMQ 11148. Q6_K native = revert candidate). decode
  BEZ ZMIAN 151, PPL 30.3113 (== 30.31 baseline), koherencja OK.** Lever 1 sam
  odzyskał całą regresję → levery 2 (fuzja rmsnorm→q8_1) i 3 (MPAD) niepotrzebne.

- 🟡 **Per-32-block Q4_K int8 GEMM (flush kernel) — ZBUDOWANY i BIT-EXACT, ale
  ~140–196 TOPS = remis/poniżej CUDA MMQ (208), NIE przełączony (2026-07-20,
  Finding M).** Odpowiedź na otwarte pytanie Finding K/L o koszt per-blokowego
  flusha. Kernel (fork multistage int8, BK=64, flush per inner mma =
  jeden 32-podblok Q4_K, skale w SMEM kb-major double-buffered) jest **bit-exact**
  vs CPU `vec_dot_q4_K_q8_1` (max_rel=0.0) — więc jakość = MMQ 30.31 z konstrukcji.
  ALE per-blokowy flush + ruch danych skal zjada 2.5–4× vs czysty int8 (587→196 na
  down-proj T=2048, 139 na gate/up). Diagnoza rozstrzygająca: stałe skale (zero
  ruchu danych) = 402 TOPS, usunięcie bariery tylko +4–7 → wąskim gardłem jest RUCH
  DANYCH SKAL (osobne tensory `dsc`/`dm`/`da`/`sa` + staging), nie spill (0 B) ani
  bariery ani ILP mma. Dodatkowo kernel czyta wagi jako rozpakowany int8 (2× pasmo
  vs upakowany Q4_K). Reguła decyzyjna → **default BEZ ZMIAN (CUDA MMQ na Ada, Mojo
  i8mma przenośne na pre-Ada); MMQ NIE wycofany.** Przenośny sm_80/sm_86 (3090)
  zweryfikowany (`ptxas -arch=sm_86`). Wszystko w scratch
  (`scratch/i8mod/multistage_i8_q4k.mojo`, `scratch/bench_i8mod_q4k.mojo`).

- 🟡 **Upakowany layout Q4_K (weights 4-bit + skale f16) — ZBUDOWANY i BIT-EXACT,
  ale upakowanie wag NIE bije MMQ; ściana to FLUSH, nie pasmo wag (2026-07-20,
  Finding N).** Domknięcie otwartego pytania Finding M (upakowany layout jako droga
  >208). (1) **skale f16** (half2, jak MMQ): unpacked int8 + per-blokowy flush z f16
  = **160–197 TOPS**, bit-exact, podnosi shape'y scale-bound (gate/up T2048 138→189)
  — NAJLEPSZY wariant, ale wciąż 0.91–0.95× MMQ (208). (2) **wagi 4-bit** (cp.async
  upakowanych bajtów do pipelined SMEM + rozpakowanie w kernelu, `b_swizzle=False`):
  **REGRESJA do 146–166 TOPS** mimo połowy pasma wag; swizzle on/off <5 % → wąskim
  gardłem NIE są konflikty banków ani pasmo wag, tylko sam flush (odczyty skal z SMEM
  + akumulacja per-element). Kernel nie jest weight-HBM-bound, więc upakowanie nie
  pomaga. Reguła decyzyjna → **default BEZ ZMIAN (CUDA MMQ); MMQ NIE wycofany.**
  Bit-exact + sm_80/sm_86. Scratch: `scratch/i8mod/multistage_i8_q4k_f16.mojo`,
  `multistage_i8_q4k_pack.mojo`, `scratch/bench_i8mod_q4k_{f16,pack}.mojo`.

- 🟡 **UNIWERSALNOŚĆ: NATIVE-LAYOUT Mojo int8 Q4_K prefill GEMM (true 1× VRAM) —
  Faza 1 WYLĄDOWAŁA in-tree, bit-exact; okablowanie + wycofanie CUDA MMQ = Faza 2
  (2026-07-20, Finding P).** Wariant PACKED (Finding O) miał 1× *bajtów*, ale wymagał
  osobnej rezydentnej kopii `[N,K/2]` + tensorów skal `dsc/dm` (ekstra VRAM ponad
  natywne bajty GGUF, których i tak potrzebuje dekodowy dp4a GEMV). Faza 1 usuwa to:
  kernel `src/modular_i8/multistage_i8_q4k_native.mojo` czyta **surowe 144-bajtowe
  superbloki GGUF `block_q4_K` w kernelu** (te SAME bajty `DevWeight::Q4K.buf` co
  dekod), de-interleave nibbli ggml + rozpakowanie ośmiu 6-bitowych sub-skal/minów
  (`get_scale_min_k4`) IN-KERNEL → skale `dsc=d·sc`, `dm=dmin·m` liczone z nagłówka,
  bez repacku i bez tensorów skal. Flush `acc += dsc·da·sumi − dm·sa` bez zmian, więc
  PPL == Q4_K MMQ (30.31) z konstrukcji. `q4k_repack_pack`, `gemm_q4k_i8_mod_pack` i
  `multistage_i8_q4k_pack.mojo` USUNIĘTE. **Zweryfikowane (RTX 4090):** bit-exact vs
  CPU golden czytający TE SAME natywne bajty (`scratch/verify_q4k_native.mojo`:
  `max_abs=0`, `max_rel=0`, `bad=0` dla 256×512×100 / 512×1024×200 / 256×14336×64);
  true 1× VRAM z konstrukcji (launch podaje tylko `w_buf` + `da/sa`, zero drugiej
  alokacji wag; smem 53 248 B); AOT-kompiluje się, `ptxas -arch=sm_86` i `-arch=sm_89`
  OK po retargecie `.target sm_80`; wrapper `gemm_q4k_i8_native[N,K,MPAD]`, drabina
  MPAD 128–4096 × 4 kształty Mistral. **Faza 2 (pozostaje, default BEZ ZMIAN):** eksport
  24 instancji PTX w `build_kernels.mojo` + `registry.rs`; launcher (bucket-select +
  zero-pad, podaje natywny `Q4K.buf`, BEZ repacku); routing `model.rs` (przerobienie
  fused rmsnorm→`block_q8_1_mmq` na `quantize_act_q8_1`); USUNIĘCIE całego CUDA MMQ
  (mmq_q4k.cu, vendor/llama-cpp, cubin, MmqScratch, fused mmq); bramki (build/clippy,
  Bielik golden, PPL≈30.3, coherence, bench, VRAM). Do czasu tego default BEZ ZMIAN.

- ✅ **Dekod Q4_K (dp4a GEMV) jest przy ścianie przepustowości — bije llama.cpp
  w płytkim kontekście, remis w głębokim (2026-07-20, nic nie wysłano).** Sesja
  celowała w rzekomą lukę 20 % (146 vs 175 tok/s) do llama.cpp. Pomiar na tym 4090
  pokazuje, że luka **już nie istnieje** — kod dekodowy poprawił się od czasu
  postawienia zadania (prefill 2827→11095 tok/s). Mistral-7B Q4_K, `bench
  --prefix-cache off --reps 5`, GPU bezczynny:
  - Płytki (prompt 8, gen 512): **FORGE 177.7 tok/s** vs llama.cpp `tg512` **169.6**
    → FORGE **+4.8 %**. (776 GB/s wg konwencji 4.37e9×tok/s.)
  - Głęboki (prompt 4096, gen 512): FORGE **149.9** vs llama.cpp `tg512@d4096`
    **152.9** → FORGE **−2.0 %** (różnica to atencja po KV, nie GEMV wagowy).
  Izolowany DRAM-bound mikrobench GEMV (bufor wag 302 MB ≫ 72 MB L2): **884 GB/s =
  87.7 % szczytu 1008**. Achievable copy-bandwidth tej karty (r+w 1 GB) to
  **884 GB/s** — GEMV chodzi **dokładnie z prędkością surowego memcpy urządzenia**,
  czyli jest w pełni wysycony pamięciowo, bez redukowalnej nieefektywności.
  Wcześniejszy `bench_decode_mistral` dawał „2779 GB/s" (>szczyt) bo bufory (66 MB
  gate|up) mieszczą się w L2 — mierzył L2, nie DRAM. Modular nie ma eksportowalnego
  GEMV pod GGUF-Q4_K (biblioteka skompilowana, matmul host-dispatch/multistage —
  nie AOT-single-PTX per ADR-0001; brak superbloków GGUF Q4_K). **Wniosek: brak
  wygranej do wysłania — GEMV jest przy ścianie i już bije referencję.** Golden
  Bielik NVFP4 bit-exact (1 i 4 lane); Q4_K greedy „Paris, France…" bez zmian.

- ✅ **Fuzja RMSNorm→q8_1 (DS4) na ścieżce MMQ prefill** (`forge_rmsnorm_q8_1_ds4`
  / `forge_rmsnorm_residual_q8_1_ds4`, `kernels/cuda/mmq_q4k.cu`): norm poprzedzający
  projekcje Q4_K q/k/v (i osobno gate/up) emituje aktywację `block_q8_1_mmq` DS4
  wprost, więc trzy GEMM-y q/k/v czytają JEDNĄ kwantyzację (2→gate/up), bez osobnego
  passa `quantize_mmq_q8_1_ds4` i bez rundy f16 przez HBM. Jeden blok/token: redukcja
  sum-of-squares f32 (dataflow jak `norm.mojo`), potem pakowanie per-32 po tej samej
  wartości f16 co standalone quant → **bit-w-bit identyczne** (`forge ppl`=30.3113 =
  baseline; tokeny identyczne; Bielik NVFP4 golden bit-exact 1 i 4 lane). Efekt
  strukturalny (nsys pp4096): launche `quantize_mmq_q8_1_ds4` **768→320**,
  `rmsnorm_residual_f16` **256→64**. Bramkowane na ścieżkę MMQ (n_tokens≥64, bez W4A8,
  bez kalibracji) i q/k/v(gate/up) wszystkie Q4_K; reszta → committed norm+quant.
  Dekod/NVFP4/W4A8/MoE nietknięte. **Wall-clock neutralne** (best-of-5: pp4096
  8569→8591, pp8192 ~8200→8243 tok/s) — standalone quant był już ~1.4 % prefill i
  nakłada się; pułap na RTX 4090 to GEMM MMQ (~56 %) + FA (~24 %), nie fuzowalny ogon
  non-GEMM (~6 %). Fuzja SwiGLU→q8_1 (down) zaimplementowana i token-identyczna (~2 %),
  ale reimplementacja `exp` w CUDA psuje bit-identyczność (ppl 30.31→30.38) → **cofnięta**
  (bramka jakości > ~2 %); down zostaje na `silu_mul_f16` + wewn. quant MMQ.

---

## Zrobione (rdzeń, jednokartowy NVIDIA, produkcyjny)

- ✅ **HAL CUDA** (cudarc): areny VRAM, streamy/eventy, CUDA graphs, pinned copy
- ✅ **Formaty**: GGUF v2/v3 (WSZYSTKIE kwantyzacje natywnie w VRAM: Q2-Q8_K,
  Q4/5_0/1, IQ1-IQ4, MXFP4), safetensors (NVFP4, FP8, BF16, sharding)
- ✅ **Tokenizery + chat templating**: HF + GGUF-BPE, minijinja HF-compat,
  streaming detok UTF-8, stop-holdback. Żądanie dokłada własne zmienne szablonu
  (`chat_template_kwargs`, jak w vLLM/SGLang) i wygrywa z tymi z checkpointu, bo
  `bos_token` opisuje model, a `enable_thinking` jedną rozmowę: Qwen3.6-35B
  odpowiada na „Say hi." preambułą rozumowania domyślnie, a „Hi! How can I help
  you today?" przy `enable_thinking: false`
- ✅ **Kernele Mojo** (AOT→PTX): rmsnorm/layernorm, rope, silu, fused dequant
  GEMV+GEMM (wszystkie quanty + dp4a int8 + mma tensor-core), paged flash
  attention (decode split-K + prefill, GQA), conv1d/gelu (Whisper), sampling GPU,
  MoE router (softmax→top-k→renorm) + scale-add akumulacja ekspertów
  - ℹ️ **Prefill GEMM Q8_0/Q4_K = int8-MMQ tensor-core** (`gemm_i8mma_impl`,
    `mma.sync.m16n8k32.s8.s8.s32` przez `inlined_assembly`+`_RegisterPackType` —
    marshalling 4×s32 rozwiązany). Aktywacja kwantyzowana do q8_1 RAZ na GEMM
    w prepassie `quantize_act_q8_1` (int8 `[T,K]` + skale block-major `[K/32,T]`
    dla koalescencji), GEMM czyta int8 X bezpośrednio → połowa pasma X + zero
    redundantnej rekwantyzacji per blok wag. Dodatkowo **reblock BN=128**
    (`gemm_i8mma_impl[BM,BN,NW,FMT]`, wariant `_big` = BM128×BN128, 512 wątków/
    16 warpów): podwaja wiersze/blok, więc aktywacja X (czytana `ceil(rows/BN)`
    razy) idzie o połowę rzadziej. Klucz do braku regresji occupancy: akumulator
    per-warp STAŁY (MT2×NT4=8), więcej WARPÓW zamiast n-tiles/warp → 127 rej. →
    1 CTA/SM = 16 warpów (identyczny footprint jak stare 2×256 wątków). **Bit-w-bit
    identyczny ze skomitowanym BM=128** (mma int8 jest dokładne — dowód per-element
    w `test_gemm_i8mma.mojo`). Bramkowany na kształt (`n_tokens≥1024` ORAZ
    `ceil(rows/128)·ceil(n_tokens/128)≥256`), bo gruby blok niedopełnia SM-y dla
    krótkich chunków (512-prefill −11 %) i małych modeli (Qwen3-0.6B rows≤3072,
    −19 %) — te zostają na skomitowanym kernelu. Efekt (A/B 3-rep, ta sama karta):
    Mistral Q4_K prefill 4096 **2588→2827 (+9 %)**, 8192 **2246→2343 (+4 %)**;
    512 i Qwen3-0.6B bez zmian; dekod bit-bez-zmian (dp4a GEMV). Mikrobench
    T=2048: 58→65 TOPS = **31 %→35 % pułapu 184-TOPS**; nsys Mistral-4096 czas
    GEMM i8mma −10.9 %. Poprawność ≈4.6e-4 vs exact CPU MMQ. Pozostałe formaty
    prefill = f16 tensor-core (dequant Q→f16 → mma). Wciąż ~4× za llama.cpp MMQ
    (11–12.8k) — luka to wydajność mma-issue GEMM przy ścianie 35 % pułapu, nie
    staging X — szczegóły w `kernels/mojo/MOJO_NOTES.md`.
  - ℹ️ **Prefill Q4_K GEMM = kernel CUDA (nvcc), WYJĄTEK od ADR-0001.** Dowód
    `docs/CODEGEN_PROOF.md`: backend Mojo trafia w ścianę ~66 TOPS na tym samym
    algorytmie int8-MMQ, gdzie ptxas/nvcc planuje identyczne `mma.sync.m16n8k32.s8`
    do >200 TOPS. `kernels/cuda/gemm_i8mma.cu` (BM128×BN128, 256 wątków, MT4×NT4)
    kompiluje się przez `nvcc -arch=sm_89 -cubin` (`kernels/cuda/build.sh`) do
    komitowanego `build/sm_89/gemm_i8mma_cuda.cubin`, ładowanego TĄ SAMĄ ścieżką
    cudarc `cuModuleLoadData` co PTX (Exp 4). Kontrakt I/O bit-w-bit zgodny z
    `gemm_i8mma_impl`: ten sam prepass `quantize_act_q8_1`, te same kody GGUF, to
    samo wyjście f16 — wynik **bit-identyczny z Mojo** (rel 0.0e0), a vs exact CPU
    MMQ ~4.6e-4 (test `crates/forge-kernels/tests/cuda_i8mma.rs`). Izolowany GEMM
    Q4_K (RTX 4090, ta sama karta): Mojo 55–65 → **CUDA 65–107 TOPS (1.6–1.9×)**.
    Prefill Mistral-7B Q4_K: 512 **2497→3334 (1.34×)**, 4096 **2956→3536 (1.20×)**,
    8192 **2477→2930 (1.18×)**; dekod bit-bez-zmian (dp4a GEMV nietknięty). Routing:
    tylko **Q4_K prefill** (`n_tokens≥64`) → CUDA; **Q8_0 zostaje na Mojo** (jego
    skomitowany i8mma ~120 TOPS jest szybszy niż ten kernel CUDA na Q8_0 — brak
    regresji), a dekod/małe batche → Mojo. `FORGE_I8MMA_BACKEND=mojo|cuda` wymusza
    backend (A/B). Luka do llama.cpp (11965) wciąż ~3.4× — ten kernel CUDA planuje
    ~107 TOPS, ~połowa dostrojonego MMQ llama.cpp (208); reszta = fuzja Fazy 2
    (attn/quant/norm nie-fused). Cubin komitowany jak PTX; ADR-0001 łamane dla
    JEDNEJ rodziny kerneli.
  - ✅ **Vendorowany Q4_K MMQ llama.cpp (`FORGE_GEMM=mmq`, 2026-07-19)** — domyka
    lukę 107-vs-208 TOPS z Exp 2/5: zamiast pisać kernel ręcznie, `kernels/cuda/
    mmq_q4k.cu` włącza NAGŁÓWKI ggml-cuda vendorowane do `kernels/cuda/vendor/
    llama-cpp/` (13 plików, 644 KB, MIT, commit `112c781`) i instancjonuje
    PRAWDZIWY `mul_mat_q` Q4_K llama.cpp (`load_tiles_q4_K`→`vec_dot_q4_K_q8_1_mma`
    →`mmq_write_back_mma` przez `mul_mat_q_process_tile`) — nvcc kompiluje ICH kod,
    nie kopię. TU dokłada tylko `extern "C"`, wrapper siatki (dense conventional
    tiling), `quantize_mmq_q8_1<DS4>` (wariant f16-in) i epilog f32→f16. Zjada
    **natywne bajty GGUF Q4_K rezydentne (BEZ rekwantyzacji, bez zmiany jakości)**
    + własny q8_1. Izolowany GEMM (RTX 4090, `scratch/mmq_probe/tops.cu`): hand
    65–109 → **MMQ 116–264 TOPS (1.8–2.4×)** = poziom 208 z proof. Prefill Mistral
    Q4_K (FA on, 3-rep steady): 512 2590→**4609 (1.78×)**, 4096 4652→**6478 (1.39×,
    0.54× llama.cpp — było 0.38×)**, 8192 4645→**6327**; dekod bez zmian (146→146).
    Poprawność: GPU-vs-CPU golden Q4_K relL2 ~3.9e-3 (`scratch/mmq_probe/harness.cu`),
    coherence token-identyczna z domyślną ścieżką. HAL `launch` włącza opt-in >48 KB
    dynamic smem (`CU_FUNC_ATTRIBUTE_MAX_DYNAMIC_SHARED_SIZE_BYTES`). **Teraz DOMYŚLNY**
    dla prefill Q4_K; Q8_0/NVFP4/dekod nietknięte, Bielik NVFP4 golden bit-w-bit.
  - ✅ **Q6_K prefill → MMQ + epilog f16-direct (DOMYŚLNE, 2026-07-19)** — dwie
    koherentne wygrane nad domyślnym Q4_K MMQ, obie czysto numeryczne (te same
    natywne wagi GGUF + q8_1, bez zmiany jakości). (1) Instancja Q6_K w
    `mmq_q4k.cu` (`forge_mmq_q6k_x*`, `load_tiles_q6_K`→`vec_dot_q6_K_q8_1_mma`
    verbatim; układ q8_1 **D4** = tylko `d`, nowy `forge_quantize_mmq_q8_1_d4`;
    smem identyczny jak Q4_K, MMQ_MMA_TILE_X_K==76) zastępuje wolny Mojo f16 GEMM
    Q6_K down-proj (był 27% prefillu). Routing prefill (`n_tokens≥64`) w
    `gemm_q6_k_f16_at`; dekod Q6_K zostaje na dp4a gemv Mojo. (2) MMQ zapisuje f16
    wprost (`forge_mmq_write_back_f16`, `__float2half`) dla Q4_K i Q6_K — usunięty
    osobny kernel `forge_f32_to_f16` (był 8%, 768 launchy) + scratch f32.
    Poprawność: Q6_K GPU-vs-CPU golden relL2 ~3.77e-3 (`scratch/mmq_probe/
    q6k_harness.cu`), Q4_K f16-epilog re-pass ~3.9e-3, coherence Mistral Q4_K
    token-identyczna z all-Mojo. Prefill Mistral (FA on, best-of-3): 512
    4609→**5851**, 4096 6478→**7956 (0.665× llama.cpp — było 0.54×)**, 8192
    6327→**7753**; dekod bez zmian (146.2/130.5). nsys: znika `forge_f32_to_f16`
    i Mojo Q6_K prefill GEMM; Q6_K MMQ 10%, quant-d4 <1%. Bielik NVFP4 golden
    bit-w-bit (1 i 4 pasy).
  - ✅ **MMQ prefill Q4_K + Q6_K → stream-K (DOMYŚLNE, 2026-07-19)** — ta sama
    ścieżka jądra/kwantu, bez zmiany jakości (koherentne). Zamiast dense
    conventional-tiling wrappera włączono PRAWDZIWĄ ścieżkę **stream-K** ggml
    (arXiv:2301.03598): kopie `mul_mat_q` (branch VOLTA+) i `mul_mat_q_stream_k_fixup`
    z `mmq.cuh`, wyspecjalizowane do dense (1 kanał/sample, `ids_dst==null`), z
    dst f16 (`forge_mmq_process_tile`/`forge_mmq_stream_k_body`/
    `forge_mmq_stream_k_fixup_body` w `mmq_q4k.cu`; compute bajt-identyczny, tylko
    dtype dst + epilog f16). Siatka = **1 blok/SM** (`nsm` z HAL:
    `CU_DEVICE_ATTRIBUTE_MULTIPROCESSOR_COUNT` → `DeviceCaps.sm_count`); bufor
    partiali `MmqScratch.fixup` (`nsm*mmq_x*mmq_y` f32, grow-only). Launcher
    `gemm_mmq_at` odpala quant → stream-K GEMM (f16 wprost) → fixup reduction
    (f16 += float na kaflach granicznych). Cel: stream-K balansuje kafle po
    wymiarze K na WSZYSTKICH SM-ach, likwidując straty częściowej fali gdy
    `ntiles % nsm != 0` (dokładnie kształt FFN up/gate — 55% prefillu wg nsys).
    Izolowany GEMM (RTX 4090, `scratch/mmq_probe/tops_sk.cu`, dense→stream-K):
    FFN up/gate N=14336 K=4096 T=512 132.9→**152.4 TOPS (1.15×)**, T=2048
    132.8→**143.4 (1.08×)**, T=4096 243.6→241.7 (0.99× = wash, kafle dzielą się
    równo na 128 SM); FFN down N=4096 K=14336 T=512 254.5→260.2 (1.02×); attn
    N=4096 K=4096 płasko (~1.0×). Narzut fixup **~0.7%** czasu stream-K GEMM
    (nsys; bloki bez pracy robią early-out). Poprawność: stream-K vs dense relL2
    **≤2.6e-4** (f16 round-off na kaflach granicznych), stream-K vs CPU golden
    **~3.8e-3** (`scratch/mmq_probe/sk_harness.cu`), Q6_K in-suite golden
    `gemm_q6_k_mmq_prefill_matches_formats_dequant` PASS, coherence Mistral Q4_K
    "Paris, France…" token-close z dense. nsys prefill (pp4096): stream-K Q4_K
    **54%** + Q6_K **10%** = 64% MMQ, fixup <1%. Whole-model pp4096 ~wash
    (8000–8890, było 7956–8466 — przy T=4096 kafle dzielą się równo, więc zysk
    jest przy mniejszych/częściowo-falowych prefillach, nie na headline'ie
    pp4096); dekod nietknięty (`n_tokens<64` = gemv/i8mma, nie MMQ). Bielik NVFP4
    golden bit-w-bit (1 i 4 pasy). `FORGE_GEMM=cuda|mojo|w4a8` fallbacki działają.
  - ✅ **Tensor-core flash-attention prefill (`FORGE_ATTN=fa`, 2026-07-19)** —
    `kernels/cuda/fattn_prefill.cu` (2. wyjątek ADR-0001, ten sam cubin path co
    `gemm_i8mma`). Zastępuje skalarny/SIMD `attn_prefill` (Mojo, `dotv += qv8*kv8`)
    **f16 mma `m16n8k16`**: QK^T przez mma, online-softmax w rejestrach (running
    max/sum, akumulator O, rescale per-tile), P·V przez mma, nad paged KV + GQA.
    Kontrakt I/O bajt-w-bajt jak `attn_prefill` → drop-in. V zapisywane
    transponowane w smem (`[head_dim][key]`), K/V ładowane `ldmatrix.x2` (bez
    `.trans`, konwencja `mma.row.col` jak w gemm), layout akumulatora S == layout
    operandu A → P·V bez repacku. hd128 157 rej, 32 KB smem, 0 spill, ~3 CTA/SM;
    hd64 też. Domyślnie **OFF** (`scalar` = Mojo, golden bit-exact). Poprawność
    vs CPU-golden (paged, GQA, causal): max_abs **~3.9e-4** (tolerancja f16), 0 zł.
    Koherencja: greedy Mistral token-w-token == ścieżka skalarna; Bielik NVFP4
    golden bit-exact na 1 i 4 liniach też pod `FORGE_ATTN=fa`. Sam kernel
    attention **3.9× szybszy** (8192 prefill: 1322→340 ms). Prefill (stack
    koherentny CUDA-MMQ GEMM + FA): 4096 **3749→4556 (1.22×)**, 8192
    **2953→4638 (1.57×)**; dekod nietknięty (osobny kernel). Luka do llama.cpp
    (pp4096 11927) 3.18×→**2.62×**. Z W4A8 (teraz KOHERENTNY, patrz niżej) FA
    składa się do pp4096 **8725**, pp8192 **8849** tok/s. `docs/BENCH_COMPARISON.md`.
  - ✅ **Portowalny Mojo tensor-core FA (`FORGE_ATTN=fa_mojo`, 2026-07-20)** —
    `attn_prefill_fa_mma` (`kernels/mojo/src/prefill.mojo`) to Mojo mirror
    `fattn_prefill.cu`: ten sam algorytm (f16 `m16n8k16` mma QK^T, online-softmax
    w rejestrach, mma P·V, paged KV + GQA + causal), ten sam kontrakt tilingu
    (BQ=64, BK=32, 4 warpy). mma/`ld_matrix[8]`/`ld_matrix[4]` z `std.gpu.compute.mma`,
    redukcja 4-lane przez `shuffle_xor`, V transponowane w smem (`[head_dim][key]`)
    → P·V czyta B non-transposed; layout akumulatora S == layout operandu A → P·V
    bez repacku (pakowanie f32-prob → SIMD[f16,8]). Poprawność vs CPU-golden
    (paged, GQA, causal, tile-tail, granica bloku BQ): max_abs **~1.2e-4**,
    max_rel **~1.1e-3**; vs skalar max_abs ~1.2e-4 (`test_fa_mma.mojo`). Koherencja
    Mistral greedy „Paris…" == fa/scalar. **WERDYKT: konkurencyjny.** nsys attn
    kernel (Mistral, isolowany czas GPU): 4096 CUDA 97.9 → Mojo 102.2 ms
    (**+4.4 %**), 8192 CUDA 349.8 → Mojo 359.2 ms (**+2.7 %**) — w progu ~15 %.
    End-to-end prefill (warm, tok/s): 512 fa 5727 ≈ fa_mojo 6053, 4096 fa 7585 /
    fa_mojo 8523, 8192 fa 8231 ≈ fa_mojo 8104. Dekod bit-nietknięty (osobny
    kernel decode, 146/130 identycznie). W przeciwieństwie do int8-GEMM
    (`CODEGEN_PROOF.md`, ściana 3.5×) struktura FA (krótka redukcja online-softmax,
    NIE głęboki K-unroll pipeline) planuje się w Mojo konkurencyjnie → **kandydat
    na portowalny default** (jedno źródło Mojo → PTX+AMDGPU+Metal, ADR-0001).
    Default nietknięty (`fa`=CUDA). `docs/BENCH_COMPARISON.md`.
  - ✅ **W4A8 prefill GEMM teraz KOHERENTNY (`FORGE_GEMM=w4a8`), wciąż non-default
    (2026-07-19).** Prawdziwą przyczyną wcześniejszego „gibberish" był BŁĄD
    rekwantyzacji, NIE outliery aktywacji: zero-point QoQ `w ≈ s1·int8(s2·(q4−zero))`
    jest pełną liczbą całkowitą (bajt `(−zero)·s2` w int8), a packer host clampował
    go do nibble `[0,15]` — poprawne tylko dla grup przecinających 0. Grupy
    nieprzecinające zera (duża część wag) zapadały się → **0.32 relL2, PPL ~311**.
    Poprawka (signed zero-point + per-row clip search na `s1`) daje **~0.02 relL2**
    na gładkich wagach; golden `cuda_w4a8` (2e-4) nadal pasuje, test regresji
    `requant_quality_and_group_monotonicity` (finer group nie może być gorszy).
    Jakość (held-out PPL, `forge ppl`, Mistral-7B Q4_K_M): Q4_K **30.31** vs W4A8
    **37.98 (+25 %)** — koherentny (Paris, wzór wody, kod Python), ale nie na parze.
    SmoothQuant zaimplementowany (kalibracja + migracja per-linear złożona w wagę,
    odwrotność w kwantyzatorze aktywacji `inv_smooth`), ale ZMIERZONY jako
    REGRESUJĄCY tę ścieżkę (α=0.5→49.9, α=0.8→42.8): rekwant Q4_K→W4A8 jest
    **weight-bound**, migracja tylko powiększa zakres per-row dla obowiązkowego
    stage-1 int8. Domyślnie identity (bez smoothingu); opt-in `FORGE_W4A8_ALPHA`.
    Perf (oba +FA, tylko GEMM się różni): pp4096 8043→**8725 (+8 %)**, pp8192
    8194→**8849 (+8 %)**, pp512 regres (CTA underfill); dekod bit-bez-zmian; NIE
    bije llama.cpp (0.73×). Werdykt: koherentny, ale +25 % PPL → NON-DEFAULT.
  - ℹ️ **Profil prefill (2026-07-19, nsys RTX 4090): compute-bound, brak narzutu
    launchy.** Suma czasu GPU kerneli = 1433.5 ms vs wall 1436 ms przy P=4096
    (luka 0.17 %; 0.8 % przy P=512) → **CUDA-graphing prefillu nic nie daje**
    (GPU nie głoduje, ~768 launchy queue'owane szybciej niż drenowane). Podbicie
    occupancy i8mma przez `.maxnreg 85` (2→3 CTA/SM) **regresuje** (4096: 2800→~2400
    tok/s — kernel jest mma-issue/ILP-bound, nie occupancy-bound). Szersze chunki
    (1024→2048) = neutralne. Nic nie wdrożono. Realna praca na lukę 4.3×: Q6_K
    down-proj przez int8-mma (~+5 %), redukcja `barrier()` per 32 kol, fused
    flash-attn prefill / megakernel (przyszłe pasy) — `docs/BENCH_COMPARISON.md`.
  - ✅ **fp8 (e4m3) prefill GEMM = PRAWDZIWY kernel Mojo, jakość near-lossless, ale
    WOLNIEJSZY e2e niż CUDA MMQ na Ada (`FORGE_GEMM=fp8`, non-default, 2026-07-20).**
    Faza-2: `kernels/mojo/src/gemm_fp8.mojo` — jedno-PTX e4m3 tensor-core GEMM
    (`mma.m16n8k32.f32.e4m3.e4m3.f32`, skala per-wiersz wag + per-token aktywacji,
    akumulacja f32, skalowanie w epilogu) + `quantize_act_fp8`. Komitowany PTX
    samowystarczalny: `build_kernels.mojo` (`_finalize_fp8`) podbija `.version`→8.4,
    więc sterownik JIT akceptuje bez runtime-shima (shim tylko dla `mojo run`
    scratchy). Kernel = exact CPU fp8 ref do 0.0012, wszystkie 3 kształty tile
    bit-identyczne (`test_gemm_fp8.mojo`). Pack Q4_K→fp8 + `build_fp8` w
    `weights.rs`/`model.rs`, route w `prefill_forward`, launcher `gemm_fp8`.
    **Jakość (held-out PPL, Mistral-7B Q4_K_M): Q4_K 30.31 vs fp8 30.52 (+0.69 %)**
    — near-lossless (jak predykcja fidelity 2.15 %, nic jak W4A8 +25 %), koherentny
    (Eiffel→Paris). Schemat skala per-wiersz+per-token WALIDOWANY: wykładnik e4m3
    absorbuje rozrzut blok-do-blok, brak potrzeby skali per-blok / kalibracji.
    **Perf (oba +FA, `--prefix-cache off`, RTX 4090): pp512 3015→2242 (−26 %),
    pp4096 8028→6472 (−19 %), pp8192 7953→6301 (−21 %); dekod bit-bez-zmian.**
    Root-cause (nsys): sam GEMM fp8 jest regresją, NIE quant aktywacji — mediana
    per-launch **fp8 GEMM 671 µs vs CUDA MMQ 267 µs (~2.5× wolniej)**;
    `quantize_act_fp8` = tylko 3 % czasu GPU. Dwie przyczyny: (1) na Ada fp8 i int8
    tensor-core mają TEN SAM peak (~660 dense TOPS/TFLOPS na 4090 — 2× fp8 dopiero
    Hopper/Blackwell), brak przewagi sprzętowej; (2) 305 TFLOPS z Finding E to
    dostrojony `multistage_gemm` Modulara (host-dispatcher, nie do AOT-PTX bez
    runtime Mojo/ADR-0001) — shippable jedno-PTX kernel ma prostszą strukturę hand
    int8-MMQ (sync smem, bez stream-K), stąd przegrywa jak Mojo int8 z ggml MMQ.
    Werdykt: fp8 NIE bije MMQ na Ada w żadnym kształcie → NON-DEFAULT, wyjątek CUDA
    MMQ NIE wycofany. Wygrałby na Hopper/Blackwell lub po przepisaniu kernela na
    cp.async-multistage + stream-K. `docs/CODEGEN_PROOF.md` Finding F.
  - ✅ **Modularowy multistage fp8 GEMM SPRODUKTYZOWANY (`FORGE_GEMM=fp8mod`,
    non-default, 2026-07-20)** — realizuje ścieżkę z Finding F/G: zamiast hand
    jedno-PTX kernela, komituje PRAWDZIWY `multistage_gemm_kernel` Modulara (deep
    cp.async multistage) jako samowystarczalny AOT-PTX. `src/gemm_fp8_modular.mojo`:
    cienki wrapper `gemm_fp8_mod[N,K]` bierze GOŁE wskaźniki + `i64 m` (każdy param
    = 1 slot 8B, kontrakt HAL), buduje `TileTensor` device-side z DYNAMICZNYM M
    (`Coord(m, Idx[K])`) → **JEDEN PTX na (N,K) obsługuje KAŻDE T** (M czytane
    runtime `c.dim[0]()`; T-buckets zbędne, zweryfikowane bit-exact też dla M=100).
    Skala per-token×per-row + cast f16 FUZOWANE w epilogu (`elementwise_lambda_fn`)
    → bez osobnego passa. 4 instancje committed (`gemm_fp8_mod_{4096_4096,1024_4096,
    14336_4096,4096_14336}`, kształty Mistral-7B), `_finalize_fp8` `.version→8.4`,
    PTX samowystarczalny (0 `.extern .func`, 64× mma e4m3, 38 cp.async). Launcher
    `gemm_fp8_modular` (grid `(⌈N/128⌉,⌈T/128⌉)`, smem 65536 opt-in), route
    `Model::gemm_fp8` gdy `weights.fp8_modular`. **Izolowany GEMM: 213–289 TFLOPS =
    1.0–1.4× MMQ (208)** (dyn-M + fused f16-scale), poprawność 4.2e-4 (zaokrąglenie
    f16). **Jakość: ppl 30.5211 vs 30.3113 = +0.69 %** (identyczna numeryka fp8 jak
    hand `fp8`), coherence PASS (Paris/Tokyo/tlen), Bielik NVFP4 golden bit-w-bit.
    **PERF: patrz Finding I — po fuzji fp8mod BIJE domyślny CUDA MMQ warm.** Stara
    tabela („0.42–0.95×") była artefaktem zimnego pomiaru (osobne procesy `forge
    bench`, GPU wracał do 210 MHz).
  - ✅ **Fuzja RMSNorm→fp8: fp8mod BIJE domyślny CUDA MMQ e2e warm (Ada, 2026-07-20,
    Finding I).** Nowe kernele Mojo `rmsnorm_fp8` / `rmsnorm_residual_fp8`
    (`kernels/mojo/src/norm.mojo`, lustro CUDA `forge_rmsnorm_q8_1_ds4`) liczą
    rmsnorm(_residual) I emitują JEDNĄ aktywację e4m3 per-token (kody [T,K] + skala
    f32 per-token) w layoucie `gemm_fp8_mod`. `prefill_forward` (ścieżka `fp8mod_fuse`)
    współdzieli ją: q/k/v czytają emit attn-norm, gate/up emit ffn-norm — przez
    `gemm_fp8_modular_prequant` (zero requantu per-projekcja); o-proj + down trzymają
    własny `quantize_act_fp8` (wejście = wyjście attn/SwiGLU, nie norm). Numeryka
    bit-w-bit jak `rmsnorm_f16`→`quantize_act_fp8`, więc PPL bez zmian (+0.69 %).
    **nsys: launche `quantize_act_fp8` spadły 1792→512** (tylko o + down; usunięto
    dokładnie 5/warstwę × 32 × 8 chunków = 1280). **PERF warm best-of-N (`forge bench
    --reps`, in-process, rep 1 zimny odrzucony): pp4096 fp8mod 12036 vs default 11050
    = 1.089×, pp8192 9633 vs 9029 = 1.067×, pp512 13252 vs 13289 ≈ remis
    (launch-bound).** Sama fuzja: +1.7–2.1 % nad pre-fuzją (która JUŻ biła default
    warm — stary „0.94×" to zimny rep 1: 8072 vs 8507). fp8mod pp4096 12036 ≈ llama.cpp
    11991. Dekod BEZ regresji (osobna ścieżka gemv: 146.0/130.5/175.3 identyczne).
    **Wyjątek ADR-0001 CUDA MMQ WYCOFANY na merit** — 100 %-Mojo GEMM bije CUDA na Ada
    warm. Rekomendacja: default zostaje CUDA MMQ (fp8mod kosztuje ~120 s requantu przy
    load + osobne pakiety e4m3 w VRAM obok Q4_K + zimny start ~0.95×); `fp8mod` awansuje
    z „izolowana-wygrana/e2e-strata" na **„e2e-wygrana na Ada, opt-in dla serwowania
    niewrażliwego na latencję"**. `docs/CODEGEN_PROOF.md` Finding I.
  - ℹ️ **Pas ILP/barier i8mma (2026-07-19): każdy lever zmierzony, NIC nie
    wdrożono (no-op lub regresja).** Zaimplementowane bit-identycznie (mma int
    jest dokładne): (1) rozdział wydania mma od epilogu f32, (2) 2 k-stage'y na
    `barrier()` (`CK=2`, 448→224 barier), (3) unroll pętli CK, (4) parowane B
    `ld_matrix.x4` (2 n-tile/instrukcja, połowa ldmatrix B). Mikrobench Q4_K
    bm128: wszystkie pomagają TYLKO przy małym T (T=128 28.8→37.9 TOPS), płasko
    ±1 % przy T≥512 (57→59 TOPS ≈ 31 % sufitu 184). Diagnostyka: usunięcie CAŁEGO
    epilogu min-korekcji q4_k jest darmowe (57.1→57.3) — epilog f32 jest w pełni
    ukryty. TOPS stały T=512→2048 i odporny na cięcia barier/epilogu/ldmatrix →
    duże-T to ściana przepustowości/pasma dla tego kształtu kafla, nie limit
    issue/latency. E2e łączny kernel płaski przy 4096/8192 i **regresuje Mistral
    512 prefill −25 %** (2346→1754): dodatkowy smem/rejestry spycha poniżej
    2 CTA/SM (ta sama wrażliwość na occupancy co `.maxnreg`). Wszystko cofnięte,
    kernel z repo zachowany. Jedyny pozostały lever to przepisanie architektury
    (BN=128 by o połowę ściąć re-read X, większe kafle rejestrowe) —
    `docs/BENCH_COMPARISON.md`.
  - ℹ️ **Studium źródeł MMQ llama.cpp + replikacja schematu (2026-07-19): luka to
    codegen Mojo, NIE algorytm — nic nie wdrożono.** Przeczytany kernel MMQ
    llama.cpp (`mmq{.cuh,-vec-dot,-load-tiles}`, `mma.cuh`, config ampere, build
    `571d0d5`): ich Ada Q4_K/Q8_0 MMQ = `mma.sync.m16n8k32.s8.s8.s32` (== nasz
    `_mma_s8`), kafel I=128×J=128, **8 warpów**, occupancy=1, **64 f32 acc/wątek**,
    smem row-major+pad (BEZ egzotycznego repacku na ścieżce mma NV), skalowanie
    per-blok-32, B przez `load_generic`, plus **stream-K**. Punkt projektowy
    IDENTYCZNY z naszym poza (a) 8w×64acc vs 16w×32acc, (b) stream-K. Zreplikowano
    (a) wprost (`[128,128,8]` = ich kształt): **~7 % WOLNIEJ** (T=2048 61 vs 66).
    mma-burst (preload B, MT×NT mma pod rząd, epilog odroczony): **TOPS bit-
    identyczny** (65.98 vs 65.97). BN=256: **nie startuje** (64acc×512thr > 65536
    rej). Nie DRAM-bound (ruch ~1.03 GB → ~1 ms vs 3.65 ms realne → issue-bound).
    **Ten sam projekt MMQ ptxas z nvcc skaluje do ~92 % sufitu mma (169 TOPS),
    backend Mojo do 36 % (66 TOPS).** Wszystko cofnięte (drzewo == HEAD). Liczby
    tej maszyny: FORGE prefill 512 **1857**, 4096 **3032**, 8192 **2473** tok/s
    (decode 146–175); Qwen Q8_0 4096 **19493**; llama.cpp pp4096 **12018** →
    **~4.0× za**. Nietknięte levery (oba za ścianą rejestrów / poza nami): stream-K
    i poprawka schedulera mma/LDS w kompilatorze Mojo.
  - ℹ️ **Głęboki comptime K-unroll SPRAWDZONY: Mojo unrolluje, ale to NIE jest lever
    (2026-07-19, `docs/CODEGEN_PROOF.md` Exp 5).** Dowód codegen obwiniał lukę 3.5×
    o ZWINIĘTĄ pętlę K Mojo (8 IMMA/ciało) vs 256 IMMA/ciało u nvcc. Test wprost:
    `gemm_i8mma_deep[...,KU,NBUF]` trzyma KU kolejnych bloków 32-kol w buforze smem
    i `comptime for`-unrolluje mma po wszystkich KU → KU×8 IMMA liniowo. **SASS
    dowodzi, że Mojo TO ROBI** (`cuobjdump -sass`, IMMA/ciało): KU=1→**8**, KU=2→**16**,
    KU=4→**32**, dokładnie liniowo; BRA nie rośnie (23→26→22), 0 spill przy 104 rej.
    Czyli 8-IMMA ciało skomitowanego kernela wynika z kafla smem (1 blok/bufor), a
    NIE z odmowy unrollowania (obala tezę dowodu). **Ale TOPS ledwie drgają:** Q4_K
    RTX 4090 big(8)/deep2(16)/deep4(32): down-proj N=4096 K=14336 T=2048 65.5/66.3/
    **68.0** (+3.8 %), T=512 62.4/64.2/**67.4** (+8 %); gate/up N=14336 K=4096 płasko
    do −2 %. 4× okno = ≤+8 % (max), ujemnie na kształtach K-lekkich — nie 3.5×.
    Wciąż ~66 TOPS vs nvcc 208. **Okno pipeline'u NIE jest wąskim gardłem; luka to
    przewaga schedulera ptxas w co-issue LDS/IMMA, której Mojo nie dorównuje.** Sufit:
    KU=8 przy BM=BN=128 wymaga 80 KB smem, ptxas odrzuca (`0x14000 > 0xc000` — statyczne
    `stack_allocation` limit 48 KB); nvcc sięga 256 IMMA/ciało przez DYNAMICZNY smem.
    deep2/deep4 bit-w-bit == skomitowany (Q4_K + Q8_0). Wszystko cofnięte, kernel
    `_big` zachowany.
  - ℹ️ **Adopcja schematu llama.cpp W KERNELU CUDA (nvcc) SPRAWDZONA: wide-tile +
    deep-unroll REGRESUJE, nie skaluje do 208 (2026-07-19).** Hipoteza zadania: ten
    kernel CUDA (107 TOPS) osiąga ~połowę MMQ llama.cpp (208) bo jest „w połowie
    dostrojony"; adopcja ich schedulingu (kafel Ada, głęboko rozwinięta pętla K,
    stream-K) miała podwoić TOPS. Test wprost: przepisany `gemm_q4k_wide_core` w
    `gemm_i8mma.cu` — WIDE KTILE (KSUB bloków 32-kol w smem naraz), preload WSZYSTKICH
    fragmentów A/B do rejestrów, `#pragma unroll` po całym kaflu → 32–64 IMMA liniowo
    (vs 8 w skomitowanym), occupancy=1 przy **255 rej/wątek + 40 KB smem — DOKŁADNIE
    profil llama.cpp** (`cuobjdump -res-usage`). Cztery warianty na RTX 4090 (izolowany
    GEMM Q4_K, ten sam mikrobench): single-buffer KTILE=128 (255 rej) **66 TOPS**,
    double-buffer KTILE=64 **72–79**, KTILE=32 **89–90** — WSZYSTKIE **poniżej**
    skomitowanego **107**. Głębszy unroll = wolniej, monotonicznie. Wynik bit-w-bit
    identyczny z Mojo (rel 0.0e0) po rozbiciu epilogu na 2 akumulacje, vs CPU MMQ
    4.65e-4 (test `cuda_i8mma.rs` przechodzi). **Potwierdza Exp 5 PO STRONIE nvcc:
    okno pipeline'u NIE jest wąskim gardłem — nawet nvcc/ptxas nie odzyskuje 208 z
    ręcznie przepisanego kernela, gdy zmienia się układ kafla/smem.** 208 osiąga
    WYŁĄCZNIE FAKTYCZNY skompilowany kernel `mul_mat_q` llama.cpp (dowód Exp 2), którego
    dosłowny lift (szablonowa `mma.cuh` tile-abstrakcja + helpery `common.cuh` + nowy
    quantize `block_q8_1_mmq` + adaptacja `write_back`/layoutu na f16 `[token][row]` +
    przeróbka launchera na nowy layout aktywacji) to duża integracja ryzykująca złoty
    test Bielika — świadomie odłożona. **Skomitowany kernel 107-TOPS zachowany jako
    najlepsza ścieżka ręczna** (drzewo == HEAD; nic nie zmienione). Realna ścieżka do
    208: vendorowanie kernela llama.cpp albo cuBLASLt int8, nie ręczny rewrite.
- ✅ **Silnik LLM**: forward, paged KV, fused decode chain, batched continuous
  decode (36× throughput), chunked prefill, admission control, CUDA-graph per bucket
- ✅ **Drabinka kwantyzacji KV**: f16 → fp8 → rot4 → rot3 (TurboQuant)
- ✅ **Tiering KV**: VRAM→RAM→NVMe, chunki 4-16MB, cross-seq eviction, overlap
  restore, streamed-in-batch, KVFlash (stały VRAM)
- ✅ **Długi kontekst**: `--ctx` do max modelu (1M osiągalny przez tiering)
- ✅ **Serwer OpenAI**: chat/completions, completions, embeddings,
  audio/transcriptions, models, healthz; SSE; tool calling (hermes/llama3);
  reasoning_content; admission 429/400
- ✅ **Modalności**: LLM, STT (własny Whisper), Embeddings (pooling, Matryoshka)
- ✅ **Sampling**: temp/top-k/top-p/min-p/penalty/seed na GPU
- ✅ **GPU sampling** (logity nie schodzą na CPU)

---

## Częściowe

- 🟡 **§6 Spekulacja (n-gram i natywne MTP) WPIĘTA w decode loop**: NgramProposer
  drafuje k tokenów z własnej historii sekwencji, silnik weryfikuje je JEDNYM
  forwardem (mini-prefill nad pozycjami draftu → `sample_batched_argmax_f32`
  per pozycja), akceptuje najdłuższy zgodny prefiks, a odrzucone pozycje KV są
  wycofywane (`KvCache::rollback`, obsługa granic stron). Wynik przy temp==0 jest
  **identyczny co do tokena** z dekodowaniem bez spekulacji tam, gdzie argmax jest
  jednoznaczny (dowód E2E: powtarzalny prompt na qwen3-0.6b — spec ON == spec OFF,
  ~1.5x szybciej, 16 akceptowanych/forward = 17 tok/forward; `forge run … --speculative on`).
  Kaskada + per-proposer acceptance stats + adaptive-disable (usypianie przy braku
  zysku) wpięte. Bramka: tylko greedy (temp==0, bez repetition penalty / host-logit
  features) na gęstej ścieżce F16 paged-KV (bez tieru / prefix-cache / hybrid / MoE);
  inne żądania cicho spadają do zwykłego dekodowania. Weryfikacja idzie NIEGRAFOWANĄ
  ścieżką prefill, więc na małym modelu opłaca się dopiero dla długich draftów
  (gate `MIN_VERIFY_DRAFT`); dla ordinary prose spekulacja nie regresuje (fallback
  na pojedynczy graf-krok). Domyślnie WYŁĄCZONA (`--speculative off` = bajt-w-bajt
  dzisiejsza pętla). Natywne MTP/NextN działa osobną ścieżką dla gęstego
  hybrydowego `qwen35`: K=2/K=3, adaptacyjny wybór budżetu i batched verifier.
  Pure MTP paruje dwa requesty z tym samym K; KV, attention, DeltaNet, decyzje i
  commit są segmentowane per lane. Stan targetu i draftu MTP jest izolowany
  per sekwencja pod jednym lease, a strony draftu pochodzą ze współdzielonego
  paged cache MTP. Router `mtp+ngram:2|3` daje
  pierwszeństwo pełnemu draftowi n-gram, dogania MTP po zaakceptowanym prefiksie,
  a na miss używa natywnego MTP; raportuje osobne liczniki obu ścieżek. Wymaga
  greedy; `max_active=2` przechodzi produkcyjny E2E admission/cancel/reuse, a
  błąd restore/rollback zatruwa i poddaje kwarantannie całą parę;
  sprawdzone wykonawczo tylko na CUDA. Braki: draft-model / EAGLE / DFlash /
  DSpark, tree-verification (spec-sampling), spekulacja stochastyczna (`temp>0`)
  oraz backendy AMD/Metal.
- 🟡 **§4.2 Rejestr architektur**: qwen3, llama, mistral, olmoe (MoE), qwen3moe
  (MoE), qwen35 (gęsty hybrid SSM z natywnym MTP) i qwen35moe
  (hybrid SSM+MoE, ✅ E2E — patrz niżej). Brak: DeepSeek (MLA), Gemma
  (sliding-window), Phi.
- ✅ **§4.2 qwen35moe (Qwen3.6-35B-A3B hybrid SSM+MoE)** — DZIAŁA E2E:
  generuje spójny tekst na RTX 4090, `forge run … "The capital of France is"`
  → „The capital of France is Paris.", a w trybie `--chat` strumień tokenów
  jest identyczny co do znaku z `llama-cli` (thinking model, pełna zgodność
  greedy). Decode ~17 tok/s (ścieżka korektnościowa, bez grafu/wsadu — patrz
  niżej; llama.cpp ~194 tok/s). Podskładniki:
  - ✅ **Rejestr architektury** (`forge-formats/arch.rs::build_qwen35moe` +
    `arch/qwen35moe.ron`): wykrywanie z GGUF, reguła warstw hybrydowych
    (`(idx+1)%full_attention_interval==0` → atencja, reszta → Gated-DeltaNet;
    dla 40 warstw atencja na 3,7,…,39), parsowanie `ssm.*`, sekcje M-RoPE
    `[11,11,10,0]`, shared expert + jego bramka, głowa MTP/NextN (warstwa 40)
    pomijana. Typy `LayerKind`, `SsmParams`, pola `Hyperparams.{ssm,rope_sections,
    full_attention_interval,attn_gated}`, `ModelDescriptor.layer_kinds`. Test
    `detect_qwen35moe_hybrid_metadata` waliduje na realnym GGUF.
  - ✅ **Referencja CPU Gated-DeltaNet** (`forge-formats/deltanet.rs`): causal
    conv1d (dowolne K) + reguła delta z bramkowaniem (autoregresyjny krok,
    dokładny port `delta-net-base.cpp`) + gated-RMSNorm + log-decay/softplus +
    L2-norm; testy numeryczne. To oracle dla kernela Mojo i silnika.
  - ✅ **Kernele Mojo** (`kernels/mojo/src/deltanet.mojo` + hd256 w
    `attention.mojo`/`prefill.mojo` + partial M-RoPE w `rope.mojo`): depthwise
    `deltanet_conv_silu_f16` (causal conv1d_k4 + SiLU, okno w miejscu),
    `l2norm_heads_f16` (L2-norm per głowa), `deltanet_gated_step_f16` (rekurencyjny
    scan Gated-DeltaNet per v-head, stan `[n_v_heads, d_state, d_state]` f32 w
    miejscu), `deltanet_gated_rmsnorm_f16`, `deltanet_log_decay_f32` (softplus·a),
    `deltanet_beta_sigmoid_f32`, `attn_decode_f16_hd256` + `attn_prefill_f16_hd256`,
    `rope_neox_partial_f16` (rotacja tylko pierwszych `n_rot=64` wymiarów). PTX +
    manifest przebudowane; launchery + wpisy w `forge-kernels` (registry.rs,
    launchers.rs), build + clippy czyste. Testy numeryczne vs `deltanet.rs`
    (`kernels/mojo/test_deltanet.mojo`): conv 7.7e-5, l2norm 5.9e-5, delta_step
    1.2e-4 / state 2.4e-7, gated_rmsnorm 4.8e-4, log_decay 9.2e-8, beta 3.0e-8 —
    wszystko w tolerancji f16.
  - ✅ **Stan SSM w silniku** (`Model.hybrid_states`): rezydentna pula slotów
    `[n_v_heads, d_state, d_state]` f32 + okno conv f16 per warstwa DeltaNet.
    Każda sekwencja dostaje lease `{slot, generation}`; event GPU chroni
    przełączenie i reuse, a ponownie użyty slot jest zerowany na streamie.
    Lease obejmuje też osobny stan draftu MTP i `SeqKv` korzystający ze wspólnego
    paged cache MTP. Test Qwen3.6 NVFP4 potwierdza parity targetu, pure MTP oraz
    MTP+n-gram dla dwóch sekwencji przeplatanych A/B, wraz z cancel i release/reuse.
    Startup atomowo rezerwuje żądaną liczbę slotów albo zwraca wymagane i dostępne
    bajty. Scheduler paruje lane'y niespekulacyjnego targetu po B2: mixery pracują
    per slot, a FFN i głowa logits używają wspólnych batch GEMM; B3 ma jeden
    seryjny ogon, a B4 wykonuje dwie pary. Jedna brama capability sprawdza
    rezydentny KV i formaty wag; tiering wybiera seryjny fallback przed mutacją
    KV. E2E zachowuje parity dla B3/B4, izoluje różne parametry samplingu per lane
    i sprawdza cancel środkowej sekwencji z ponownym użyciem slotu.
    Verifier przechowuje osobne grafy T=3/4 dla każdego stabilnego identyfikatora
    slotu. Profil `nsys` dla długiego przebiegu wykazał 2 capture i 46 replay przy
    `max_active=1` oraz 4 capture i 96 replay przy `max_active=2`.
    Native MTP ma osobny same-K B2 z batchowym draftem i segmentowanym verifierem
    `[B,T]`; różne K, n-gram i tiering zachowują seryjny fallback.
    Warstwy atencji używają paged KV.
  - ✅ **Wagi hybrydowe** (`weights.rs::load_hybrid`): `LayerMixer::{Attention,
    DeltaNet}`, atencja z bramkowanym Q (szerokość `2·n_heads·head_dim`, split,
    bez fuzji), zestaw DeltaNet (in-proj/conv1d f16/dt-bias+A f16/beta+alpha proj/
    ssm-norm/out-proj), MoE z bramką shared expert (`ffn_gate_inp_shexp`). Tabela
    embeddingów trzymana host-side (gather per token), by 22 GB kwantowanych wag
    zmieściło się w VRAM 24 GB (`--weights-pool-gb 20`).
  - ✅ **Forward hybrydowy w silniku** (`model.rs::hybrid_forward_token` +
    `hybrid_attn_mixer`/`hybrid_delta_mixer`): dispatch per-`LayerKind`, bramkowana
    atencja hd256 (deinterleave q/gate → QK-norm per głowa → partial M-RoPE
    n_rot=64 → paged decode → `attn ⊙ σ(gate)` → o-proj), ścieżka DeltaNet
    (in-proj → conv+SiLU → split q/k/v → L2-norm → repeat 16→32 blokowo jak
    `ggml_repeat` → log-decay/beta → gated step → gated-RMSNorm → out-proj), MoE
    z bramkowanym shared expertem. Prefill = sekwencyjny scan rekurencyjny po
    tokenach promptu; decode = jeden token/krok. Bramka osiągnięta: spójny tekst +
    pełna zgodność greedy z `llama-cli`.
  - ✅ **KV-tiering / KVFlash dla hybrydy** (`hybrid_attn_mixer` z `AttnSrc`,
    `prefill_hybrid`/`step_streamed` tier-świadome): z 41 warstw tylko ~10 to
    atencja (paged KV) — `TierManager` dostaje listę warstw atencji i pakuje
    chunki wyłącznie z nich (indeks kompaktowy), 30 warstw DeltaNet trzyma
    rezydentny stan SSM (nigdy nie paged). Spilled atencja strumieniowana per
    warstwa (staged path, te same kernele → bit-identyczność z przebiegiem bez
    tieru). Dowód: prompt 8k z igłą, `--kv-tier nvme --kv-pages 64` (2048
    tokenów gorące) → ~6k tokenów KV atencji spilnięte na NVMe, igła odzyskana,
    ids bit-identyczne z full-VRAM, VRAM stały; `--kvflash --kv-hot-pages 64`
    bez OOM na modelu 20 GB. Nie-hybrydowe MoE (OLMoE/qwen3moe) nadal bez tieru.
  - ✅ **Zwarty target KV dla hybryd**: globalny indeks warstwy jest mapowany na
    indeks fizycznego slabu wyłącznie dla `LayerKind::Attention`. Qwen3.6-27B
    alokuje 16 par K/V zamiast 64, czyli 64 KiB/token F16 zamiast 256 KiB/token;
    prefill, decode, verifier MTP i tiering używają tej samej mapy. Cache MTP
    pozostaje osobny i jednowarstwowy, a modele dense zachowują mapę identity.
  - 🟡 **Wydajność ścieżki hybrydowej**: korektność najpierw. Router MoE +
    bramka shared-expert NIE robią już host round-tripu (device-side grouped
    dispatch `_gidx`, patrz §4.4) — per-warstwa MoE decode jest teraz bez
    `synchronize` (poza warstwami z fallback-kwantem Q8_0). Zostają: host gather
    embed per token, DeltaNet skanowany per token wieloma małymi `device.copy`,
    brak grafu CUDA i wsadu. Optymalizacja (graf decode hybrydy, wsadowy prefill)
    to follow-up.
- 🟡 **§9.2 Odporność**: admission ✅; brak respawn workera po crashu, health
  per-GPU, pełnego graceful drain.
- 🟡 **§8.3 Operacyjność**: /healthz ✅; **metryki Prometheus ✅** (`GET /metrics`,
  poza bramką API-key jak /healthz, format text 0.0.4); brak OTel, hot reload.
  Eksport realnego stanu silnika (nic syntetycznego): liczniki requestów
  (started/finished/errored), tokeny prompt/generated, `cache_read_tokens`
  (trafienia prefix-cache §5.2), akceptacje spekulacji (§6), gauge active/queued
  sekwencji i KV pages (total/used), histogramy TTFT / inter-token latency /
  decode tok/s (per request), oraz `forge_http_requests_total{route,status}`.
  Silnik trzyma `Arc<EngineMetrics>` (atomiki + histogramy bez locka), wątek
  workera aktualizuje in-place, handler /metrics tylko czyta. Dowód:
  `tests/e2e_api_surface.rs` (po generacji `requests_finished` i
  `generated_tokens_total` rosną, histogram TTFT ma obserwacje, licznik HTTP
  rejestruje /v1/messages).
- 🟡 **§1.2 Cele wydajności**: część spełniona jednokartowo (decode ≥ vLLM na
  niektórych, prefill ≥ llama.cpp); cele multi-node (RoCE 88%) nieosiągalne bez §7.

---

## Nietknięte (duże filary)

### Sprzęt / skala
- ❌ **§3 HAL multivendor**: TYLKO CUDA. Brak ROCm/HIP (AMD), Metal (Apple),
  Level Zero (Intel), CPU-compute. To rdzeń obietnicy "uniwersalny" — 0%.
- ❌ **§3.3 Komunikatory**: NCCL/RCCL/oneCCL/ForgeCCL — 0%.
- ❌ **§7 Równoległość**: TP / PP / EP / multi-node / disaggregation — 0%.
  Silnik jest ściśle jednokartowy.

### Kompilator / IR
- ❌ **§4.1 Graph IR + kompilator**: brak deklaratywnego op-grafu, passów
  (fuzja, layout planning), autotunera. Forward jest ręcznie napisany.

### Modalności i modele
- ❌ **§4.3 TTS** (silnik LM+vocoder)
- ❌ **§4.3 T2I / diffusion** (SDXL/Flux, scheduler krokowy)
- ❌ **§4.3 Video** (rozumienie + DiT)
- ❌ **§4.3 Reranking** (cross-encoder)
- ❌ **§4.3 Multimodal input** (vision encoder → embeddingi)
- 🟡 **§4.4 MoE**: routed Mixture-of-Experts DZIAŁA (OLMoE-1B-7B e2e, spójny
  tekst). Router GPU (softmax-over-all → top-k → opcjonalny renorm, test vs CPU),
  akumulacja `moe_scale_add`. Wspiera full-vector QK-norm (OLMoE) i per-head
  (qwen3moe), shared experts z bramką sigmoid (qwen35moe).
  - **Device-side grouped expert dispatch (decode)**: wybrane przez router
    ids/wagi ZOSTAJĄ na GPU i sterują GEMV-ami ekspertów przez kernele `_gidx`
    (`gemv_q4_k_dp4a_f16_gidx`, `gemv_q6_k_f16_gidx`, `moe_scale_add_gidx_f16`,
    `moe_sigmoid_f16_to_f32`) — offset wiersza eksperta `ids[j]*rows_per_expert`
    czytany W KERNELU, waga `weights[j]` też. **ZERO `device.synchronize()` w
    ścieżce decode per warstwa** (dawniej: readback ids/wag + sync KAŻDĄ warstwę,
    serializujący decode). Bramka shared-expert liczy sigmoid na GPU zamiast
    host-readbacku. Bit-identyczne z dawną ścieżką (OLMoE i qwen35moe: greedy
    output token-for-token identyczny before/after). Kwanty bez wariantu `_gidx`
    (np. Q8_0 down w qwen35moe blk.40/41) wpadają w fallback z readbackiem —
    poprawność zachowana, tylko te warstwy synchronizują.
  - **CUDA-graph**: nie-hybrydowe MoE w pełni `_gidx` (OLMoE, qwen3moe) jest teraz
    graf-capturowane (`decode_moe_graph`) — statyczna sekwencja launchy sterowana
    danymi na urządzeniu. Model z fallback-kwantem lub hybryda qwen35moe (host
    round-tripy DeltaNet) idą ścieżką per-step.
  - Pomiar RTX 4090 (single stream, temp 0, decode tok/s, before→after):
    OLMoE-1B-7B 146→157 (+7%, głównie z grafu), qwen35moe-35B-A3B 50.3→51.4
    (+2.2%; hybryda GPU-bound, sam brak sync daje mało, graf jej nie obejmuje).
  - Prefill: nadal per-token pętla z readbackiem (poprawność-first). KV-tiering:
    ✅ dla hybrydy qwen35moe (warstwy atencji), ❌ dla nie-hybrydowego MoE.
    TODO (perf): grouped-GEMM permute/unpermute, batched-MoE decode, graf dla
    ścieżki hybrydowej, KV low-bit dla MoE.
- ❌ **§4.4 MLA** (DeepSeek), **sliding-window + sinks**, **linear/SSM** (Mamba)
- ✅ **ONNX** (import grafu → IR + wykonanie GPU; `forge-onnx`): własny parser
  wire-format protobuf (ModelProto/GraphProto/NodeProto/AttributeProto/
  TensorProto, bounds-checked — granica zaufania §9.5) → lekki typowany IR
  (węzły, krawędzie po nazwach, inicjalizatory, podgrafy). Hybrydowy interpreter:
  ciężka arytmetyka (Conv1d, LSTM, Relu/Sigmoid/Sqrt, Add, Pow, ReduceMean) na
  GPU przez natywne kernele Mojo f32 (`onnx_ops.mojo`: conv1d_f32, lstm_f32,
  relu/sigmoid/sqrt/add/pow/reduce_mean_f32); operacje kształtu/kontroli (Shape,
  Gather, Slice, Concat, Reshape, Transpose, Pad-reflect, Cast, Equal, Not, If z
  podgrafami sr/init-state) na hoście — jak w produkcyjnych runtime'ach ONNX.
  **Bramka numeryczna (twarda):** Silero VAD (`silero_vad.onnx`, 25 typów op,
  689 węzłów) uruchomiony na RTX 4090 — prob. mowy `forge` vs `onnxruntime`
  (CPU EP, ten sam wejściowy frame): sine 0.2987515 vs 0.2987524, cisza
  0.0442625 vs 0.0442627 (|Δ| ~1e-6 « tol 1e-3). CLI: `forge onnx-run`.
  Depth-Anything-V2 / jina-embeddings ONNX: parser je czyta (więcej opów do
  dodania w interpreterze — łatwo rozszerzalny przez `dispatch`).

### KV / cache zaawansowane
- ✅ **§5.2 Radix-tree prefix caching** (dedup system-promptów/few-shot/multi-turn):
  drzewo radix na granularności strony (`forge-engine/src/prefix.rs`), pożyczka
  najdłuższego wspólnego prefiksu (refcount, read-only) przed prefillem + donacja
  własnych prefill-stron po zakończeniu; LRU eviction refcount-0 liści. Admission
  rezerwuje logiczny przyszły wzrost aktywnych sekwencji, respektuje
  `max_pages_per_seq` i przegląda ograniczone okno kolejki z agingiem. Przypięte
  strony pożyczonego prefiksu są nadal rozliczane konserwatywnie w pełnym budżecie
  requestu, więc współdzielenie może nie przyspieszyć admission. Cache jest
  aktywny dla verbatim `f16`/`fp8` bez
  tieringu (hybryda tylko `f16`: jej kernele uwagi i prefillu nie mają wariantu
  fp8, a `attn_prefill_fp8` istnieje dla head_dim 64/128, nie 256) (`--prefix-cache on|off`, default on). Arch hybrydowa też, o ile ma
  jedną rangę: węzeł drzewa niesie wtedy dodatkowo CHECKPOINT stanu DeltaNet
  (okno splotu + macierz stanu każdej warstwy rekurencyjnej), bo same strony
  opisują tylko attention. Pożyczka staje wyłącznie na węźle z checkpointem,
  prefill utrwala stan co `HYBRID_STATE_CHECKPOINT_STRIDE`=512 tokenów i na
  końcu promptu, a checkpointy biorą sloty z TEJ SAMEJ puli co żywe sekwencje
  (`HYBRID_STATE_CACHE_SLOTS`=16) — nie ma proporcji do strojenia. DEKODOWANIE
  też oddaje swoje strony: sekwencja prowadzi JEDEN toczący się checkpoint,
  nadpisywany na każdej granicy strony za promptem, więc wygenerowana odpowiedź
  jest prefiksem następnej tury zamiast być dla drzewa niewidoczna (koszt to
  jedna kopia D2D co 32 kroki, niezależnie od długości odpowiedzi). Strony i
  checkpointy mają osobne listy LRU: checkpoint wolno eksmitować z dowolnego
  węzła, strony tylko od liścia, a liść bez checkpointu idzie PIERWSZY, bo dla
  hybrydy jest stroną, do której żadna pożyczka nie dosięgnie. Dowód
  (GB10, Qwen3.6-35B MXFP4): powtórzony prompt 2000 tok. 1505→88 ms TTFT, ten
  sam korpus z innym pytaniem 86 ms, tokeny bit-identyczne z zimnym; pięć pytań
  do jednego inwentarza 3592/1557/94/82/89 ms. Bramka
  `hybrid_prefix_gpu.rs`: 992/1001 tok. z cache'u, prefill 507→58 ms, ten sam
  ciąg 24 tokenów, a rozjazd w połowie promptu pożycza checkpoint pośredni (512),
  a następna tura sięga 1088/1097 tok. — poza prompt, w odpowiedź. Kontynuacja
  promptu 3008 tok. jego własnym dopełnieniem 400 tok. idzie z cache'u w 3392 z
  3419 zamiast 3008, TTFT 407,5→98,2 ms. `fp8` też: Bielik-7B Q8_0 z
  `--kv-cache fp8`, trzy pytania na wspólnym inwentarzu 1302/82/82 ms wobec
  1305/1306/1307 ms z `--prefix-cache off`, hashe odpowiedzi identyczne.
  Dispatch NVFP4 małych batchy NIE używa już rodziny `gemv_nvfp4_gguf_q8_1_b*`:
  kwantowała aktywację do q8_1, podczas gdy pojedynczy token liczy w f16, więc
  tokeny sekwencji zależały od tego, czy scheduler sparował ją z inną. Artefakty
  istnieją tylko dla sm_121a, więc 4090 nie mogło tego zobaczyć. Usunięcie jest
  też SZYBSZE: `prequant_q8_1` na projekcję na warstwę kosztuje przy T=2-4 więcej,
  niż daje jednokrotny dekod wag — ThinkingCap-Qwen3.6-27B NVFP4 na GB10, MTP K3:
  decode 33,8 zamiast 24,4 tok/s (+39%), hashe identyczne. `hybrid_state_pool_gpu`
  na NVFP4: 29/32 (było 25/32); zostają trzy porównania stanu DRAFTU B1 wobec B2
  przy K=2 — pełne ID już się zgadzają, więc różnica dotyczy trafności propozycji,
  nie wyjścia modelu.
  Graf kroku hybrydy jest kluczowany SLOTEM stanu DeltaNet: przechwycenie
  zapieka bufory slotu aktywnego w tamtej chwili, więc jeden graf odtwarzany dla
  wszystkich liczył drugą sekwencję na cudzym stanie rekurencyjnym (dowód:
  `FORGE_HYBRID_DECODE_GRAPH=0` naprawiał przeplot, a graf jest domyślnie
  włączony). Kontrakt batcha wymaga formatu z DOKŁADNYM kernelem małego batcha
  (F16, Q8_0, NVFP4) — K-kwanty kwantyzują aktywacje do q8_1 i rozjeżdżają się z
  seryjnym o 0,18 przy tolerancji 0,125. `hybrid_state_pool_gpu` na
  ThinkingCap-Qwen3.6-27B Q4_K_M (GB10): 32/32.
  Natywne MTP działa OBOK prefiksu (ThinkingCap-Qwen3.6-27B Q4_K_M, GB10:
  1070 ms TTFT powtórki i 28,2 tok/s decode, wobec 6392 ms/29,7 przy samym MTP i
  1076 ms/10,9 przy samym prefiksie, hashe identyczne we wszystkich trzech).
  Wymagały tego trzy rzeczy: reset stanu draftu MTP przeniesiony do czyszczenia
  slotu (sekwencja z pożyczki nie przechodzi przez pozycję zerową), unieważnianie
  toczącego się checkpointu przy rollbacku weryfikacji oraz niebranie checkpointu
  z pozycji, do której `len` wyprzedził listę tokenów. Hybrydowy verifier odmawia
  już tylko tieringu. n-gram działa obok prefiksu tak jak dotąd. Usage
  `prompt_tokens_details.cached_tokens`. Współdzielenie CAŁYCH stron → borrower
  nigdy nie pisze do współdzielonej strony (bez CoW granicznej strony). Dowód
  (RTX 4090, qwen3-0.6b): wspólny prefiks 2048 tok. → `cache_read=2016`, prefill
  68.8→14.8 ms (**4.7×**), id bit-identyczne z cold ORAZ z `off`; multi-turn
  reużywa KV poprzedniej tury; golden Bielik NVFP4 z `off` bez zmian
  (`tests/prefix_cache.rs`, `prefix::tests`).
- ❌ **§5.2 Copy-on-write KV** (beam/n-best), MLA latent cache
- ❌ **§5.4A Expert streaming** (tiering wag MoE, Colibri) — czeka na MoE
- ❌ **§5.4B Trwałe sesje KV** (jawne, klient-podane `session_id`) —
  **świadoma decyzja: NIE implementujemy jawnego mechanizmu sesji teraz**, bo
  byłby to redundantny, równoległy tor do już istniejącego radix prefix-cache
  (§5.2), a to łamie regułę „bez duplikującej ścieżki". Uzasadnienie: realny
  przypadek multi-turn chat (tura N = prefiks tur 1..N-1 + nowy user msg) jest
  już pokryty — prefix-cache automatycznie POŻYCZA najdłuższy wspólny prefiks
  KV, więc tura 2 raportuje `cached_tokens` obejmujące turę 1 (udowodnione:
  „multi-turn reuse" w §5.2, `tests/prefix_cache.rs`). Jedyne co dołożyłby jawny
  `session_id` to PINOWANIE prefiksu przeciw eksmisji — a eksmisja zachodzi tylko
  pod presją KV i re-prefill jest poprawny (borrower produkuje bit-identyczny
  wynik). Wpięcie jawnych sesji miałoby sens dopiero razem z §5.4B tieringiem
  (persystencja KV na RAM/NVMe między turami rozłożonymi w czasie) i §9.3
  izolacją per-tenant — wtedy `session_id` staje się uchwytem do przypiętego,
  stieryzowanego prefiksu z TTL, a nie samodzielnym cache'em. Do tego czasu
  prefix-cache jest jedyną, wystarczającą ścieżką reużycia KV.
- ❌ **§5.3 GDS/cuFile**, hot-swap modeli, **multi-LoRA** (S-LoRA)

### API / serwowanie
- ✅ **§8.1.2 Constrained decoding** (JSON-schema / regex / EBNF-GBNF) — `forge-grammar`:
  jeden byte-level automat (llama.cpp-kompatybilne GBNF; JSON Schema i regex → ten
  sam automat), per-sekwencja `GrammarMatcher` liczy maskę logitów (token dozwolony
  ⇔ jego bajty utrzymują gramatykę spełnialną, z obsługą fragmentów UTF-8 /
  byte-fallback), maska ustawia `-inf` PRZED próbkowaniem (greedy i stochastyczne).
  Cache masek per stan + prefiltr pierwszego bajtu. Wpięte w API: `response_format`
  `{json_object|json_schema|regex|grammar}`, GBNF passthrough (`grammar`),
  `tool_choice` `required`/named → gramatyka wymuszająca poprawne wywołanie (znosi
  dawne 400). Ścieżka nieograniczona bit-identyczna (golden Bielik NVFP4 bez zmian).
  Dowód (RTX 4090, qwen3-0.6b Q8_0, `tests/e2e_constrained.rs`): JSON-schema
  `{name,age}` 5/5 promptów (w tym adversarialne) = 100% poprawnego JSON pasującego
  do schematu; regex daty `\d{4}-\d{2}-\d{2}` = 100%; `tool_choice required` =
  poprawne wywołanie 3/3. Koszt: ~48 tok/s constrained vs ~800 tok/s unconstrained
  (CPU sampler + skan słownika; v1 correctness-first). Ograniczenia subsetu JSON
  Schema — patrz INFER_CONFIGURATION.md.
- ❌ **§8.1.2 Prompt caching** jako kontrakt API (cache_control/prompt_cache_key)
- ✅ **§8.1.2 Kompletność API generacji** — `logit_bias`, `min_tokens`, `logprobs`/
  `top_logprobs`, `echo`, `n` (wiele completions):
  - `logit_bias` (`{token_id: bias}`, [-100, 100]; ±100 ≈ twardy force/ban) — dodawany do
    logitów PRZED próbkowaniem; `min_tokens` — tłumi wszystkie EOS (logit → -inf) aż
    sekwencja wyprodukuje próg; `logprobs` — log-softmax na hoście, per-token log-prob +
    top-N alternatyw (chat `logprobs`+`top_logprobs`, completions `logprobs:N`, kształt
    OpenAI z `bytes`); `echo` (completions) — doklejenie promptu (tokeny promptu w
    `logprobs` z `null`). Każda z tych funkcji wymusza sampler CPU (pełne logity na
    hoście, jak maska gramatyki); żądanie bez żadnej z nich zostaje na samplerze GPU —
    ścieżka bit-identyczna (golden Bielik NVFP4 `batched_bielik` bez zmian).
  - `n` — n niezależnych completions per żądanie (osobne sekwencje, ziarna
    `seed+i·φ`; dzielą prefiks promptu przez radix prefix-cache), zwracane jako
    `choices[0..n]`; non-streaming (streaming przy `n>1` = 400, tak samo `echo`/`logprobs`
    w streamie completions). Zniesiono dawne `n>1 → 400`.
  - Dowód (RTX 4090, qwen3-0.6b Q8_0, `tests/e2e_generation.rs`): `logit_bias` +100 na
    " London" → "London", -100 na " Paris" → " located"; `min_tokens` 20 → 99 tokenów;
    `echo` dokleja prompt; `logprobs` 8 poprawnych wpisów (wartości ≤0, top-1 = token
    próbkowany przy temp 0, masa prawdopodob. top-N ≤ 1); `n=3` = 3 różne, deterministyczne
    completions. Testy jednostkowe: `sample.rs` (bias/min_tokens/log-softmax),
    `api.rs` (walidacja `n`/`logit_bias`/`min_tokens`/`logprobs`/`echo`).
- ✅ **§8.1 Anthropic API** (`POST /v1/messages`) — warstwa translacji nad TĄ
  SAMĄ ścieżką generacji co `/v1/chat/completions` (żadnego równoległego
  generate). Request Anthropic (`system` string/bloki, `messages` z content
  string/blokami, `max_tokens`, `stop_sequences`, `temperature`/`top_p`/`top_k`,
  `stream`) → `Vec<ChatMessage>` + `GenerationSpec` → `start_generation`.
  Non-stream: `{id,type:"message",role:"assistant",content:[{type:"text",text}],
  stop_reason,usage:{input_tokens,output_tokens}}`. Streaming: pełna sekwencja
  SSE `message_start` → `content_block_start` → `content_block_delta{text_delta}`
  → `content_block_stop` → `message_delta{stop_reason}` → `message_stop`.
  Mapowanie `stop_reason`: EOS→`end_turn`, limit→`max_tokens`, stop-sekwencja→
  `stop_sequence` (rozróżnienie `Eos` vs `Stop` z surowego `FinishReason`, bo
  string OpenAI zwija oba do "stop"). `<think>` zdejmowany przez `OutputParser`.
  Dowód: `tests/e2e_api_surface.rs` (non-stream + stream spójny tekst, poprawne
  usage i trzy mapowania stop_reason). Braki: bloki `tool_use`/`tool_result`,
  `thinking` bloki, `/v1/messages/count_tokens`. Images endpoint nadal ❌.
- ❌ **§8.2 FORGE-RPC** (QUIC + CBOR, SDK Rust/Py/TS)
- ❌ **§8.4 Realtime API** (voice-to-voice duplex, barge-in)
- 🟡 **§8.5 Batch / offline API**: `POST /v1/completions` przyjmuje `prompt` jako
  tablicę stringów LUB tablicę tablic token-id (batch); każdy prompt × `n`
  completions jest submitowany RAZEM (wszystkie `engine.submit` przed pierwszym
  `await`), więc scheduler admituje je do jednego decode-batcha zamiast serializować.
  `choices[]` z bieżącym `index` prompt-major, usage agreguje (prompt liczony raz
  na prompt, completion tokeny sumowane). Streaming odrzucany gdy prompts×n > 1
  (400). Dowód: `tests/e2e_api_surface.rs` — 4 prompty w jednym żądaniu → 4 spójne
  choices w ~0.14 s (batched decode). Braki: asynchroniczne joby JSONL, kolejka
  throughput per tenant.

### Produkcja
- ❌ **§9.3 Multi-tenancy**: OIDC/JWT, kwoty/rate-limit, fair-share scheduler,
  izolacja prefix-cache per tenant
- ✅ **§9.4 forge pull** (HF Hub download: GGUF + snapshot safetensors, gated
  `--token`/`HF_TOKEN`, resume przez HTTP Range, weryfikacja sha256/rozmiaru,
  zapis atomowy `.part`); ❌ **auto-planner**, ❌ **forge convert** (kwantyzator)
- ❌ **§9.5 Dystrybucja**: obrazy OCI, pakiet pip, podpisy artefaktów, SBOM, fuzzing
- ❌ **§10 Bramki jakości CI**: lm-eval-harness, PPL gate, nightly benchmark farm

---

## Ocena skali pozostałej pracy (zgrubnie)

Pozostała praca dzieli się na dwie kategorie: (a) **wymaga sprzętu / modeli,
których nie ma na tej maszynie** — nie budujemy tego na ślepo, bo bez walidacji
byłby to stub łamiący regułę „zero zaślepek"; (b) rozszerzenia budowalne
jednokartowo, ale poza obecnym zakresem. Backlog walidowalny na pojedynczym
RTX 4090 (Ada) jest w praktyce wyczerpany — pozycje ✅ niżej wylądowały i mają
twarde bramki (golden bit-identyczność, e2e, numeryka vs referencja).

| Obszar | Rozmiar | Blokada / status |
|---|---|---|
| Multivendor HAL (ROCm/Metal/Intel) | bardzo duży | **brak sprzętu** — potrzeba realnego GPU AMD/Intel/Apple; osobny backend + rekompilacja kerneli Mojo per target, niewalidowalne tutaj |
| Multi-node TP/PP/EP + ForgeCCL | bardzo duży | **brak fabric/wielu węzłów** — cały pillar §7, niewalidowalny na jednej karcie |
| Modalności TTS/T2I/Video | duży (każda) | **brak zaseedowanych modeli** — osobne silniki (LM+vocoder / scheduler dyfuzji / DiT); brak checkpointów w `.runtime` |
| Graph IR + kompilator + autotuner | duży | budowalne, poza zakresem — zamienia ręczny forward (ONNX `forge-onnx` to jego zalążek) |
| Expert streaming wag MoE (§5.4A, Colibri) | średni | budowalne — tiering wag ekspertów, follow-up do device-side dispatch |
| FORGE-RPC / Realtime / multi-tenancy | średni (każdy) | budowalne, produkcyjne rozszerzenia |

Zrobione w tej rundzie (wszystko z twardą bramką, jednokartowo): radix prefix
cache ✅, constrained decoding ✅, kompletność API generacji ✅, spekulacja n-gram
wpięta w decode ✅, device-side dispatch MoE + graf decode ✅, ONNX loader
(`forge-onnx`, Silero VAD vs onnxruntime) ✅, metryki Prometheus + Anthropic
`/v1/messages` + batch completions ✅.

Wniosek: rdzeń jednokartowego LLM/STT/embeddings/MoE/ONNX jest mocny,
produkcyjny i bit-dokładny. To wciąż ~1/3 zakresu spec, ale pozostała 2/3 to
niemal w całości „całe pillary" wymagające **innego sprzętu** (multivendor,
multi-node) lub **modeli, których tu nie ma** (TTS/T2I/Video) — świadomie
nie budowane jako stub, do domknięcia na docelowym sprzęcie z realną walidacją.

## Marlin W4A16 (Phase A go/no-go, 2026-07-19) — NO-GO, nie zintegrowano

Próba obejścia plateau ręcznych GEMM-ów (Mojo 66 / hand-CUDA 107 TOPS) przez
adopcję Marlina W4A16 (kernel 4-bit, którego używa vLLM). Marlin zbudowany
standalone `nvcc -arch=sm_89`, zmierzony na dokładnych kształtach FFN Mistral-7B
(`scratch/marlin/`, poza drzewem repo). Wynik: Marlin plateauuje na **~174 TFLOP/s**
przy T=2048/4096 — to sufit rdzeni tensor **fp16** (fp32-accumulate) na 4090, kernel
jest już wysycony. llama.cpp robi te same GEMM-y na rdzeniach **int8** (MMQ,
`mma.s8`, ~206 TFLOP/s efektywnie), więc Marlin jest **wolniejszy od referencji na
tych samych mnożeniach**. Projekcja e2e pp4096: realnie ~7600, optymistycznie 10400,
sufit (zero narzutu, niefizyczny) 12460 — nigdy pewnie nie przekracza 12000.
W4A16 wygrywa tylko w reżimie memory-bound (mały batch/decode), nie w compute-bound
prefill. Droga do pobicia llama.cpp zostaje na int8 (vendorowanie `mul_mat_q`,
cuBLASLt int8, albo W4A8/Marlin-QQQ). Committed kernel `gemm_i8mma_core` bez zmian
(drzewo == HEAD). Szczegóły + surowe liczby: `docs/BENCH_COMPARISON.md`.

## W4A8 (Phase A go/no-go, 2026-07-19) — **GO** (zprojektowane pobicie llama.cpp), NIE zintegrowano

Kontynuacja po NO-GO Marlina W4A16. Kernel QServe `w4a8_per_group` (int4-waga ×
int8-aktywacja, MIT) zbudowany standalone `nvcc -arch=sm_89`, torch usunięty,
zmierzony na dokładnych kształtach FFN Mistral-7B (`scratch/w4a8/`, poza drzewem;
harness z sustained-warmup, best-of-30). Fakt sprzętowy: na 4090 `s4.s4` mma =
**1428 TOPS = 2× `s8.s8` (714)**, ale int4×int8 nie ma natywnego mieszanego mma →
W4A8 podnosi wagę int4→int8 i używa `s8` (sufit 714, ten sam co MMQ); przewaga
W4A8 to *czystszy dequant/pipeline*, nie więcej mma.

Wynik Phase A (TFLOP-eq, ten sam boost clock 2775 MHz): QServe W4A8 **~450 przy
T≥2048** i **~400+ przy T=512** — **2.2× llama.cpp (206)** i **4.0× committed FORGE
CUDA (110)**. Pierwszy kernel przekraczający ścianę 206. Projekcja e2e (GEMM=81%,
4.0×): prefill ×2.55 → pp4096 **~9650** vs llama.cpp **12032** (potwierdzone na
czystym, zimnym GPU — wcześniejsze 7475 to był artefakt kontencji/throttlingu) =
**0.80× (WCIĄŻ PONIŻEJ)**. Sam W4A8 GEMM nie wystarcza: narzut nie-GEMM FORGE
(attention prefill + quant + norm + launche) to ~3× llama.cpp — pobicie wymaga
W4A8 GEMM **oraz** fuzji/cięcia narzutu nie-GEMM. Oba wykonalne, oba to realna praca.

Phase B (dokładność, CPU, realne tensory): requant Q4_K→int4 per-group **G=128**
(wymóg kernela) = **~10.2% relL2** (asym) ponad Q4_K; QoQ QServe (int8 scale grupy)
byłby ≥10%. Nietrywialna degradacja, koherencja niezmierzona (wymaga Phase C).

Phase C **zielone światło, ale NIE wdrożone** w tej sesji: poprawny build to duży,
ryzykowny nakład (8-D interleave wag QServe + QoQ requant z Q4_K + per-token int8
act-quant + routing), walidowalny tylko golden-CPU + oracle koherencji — nie do
wysłania w połowie (reguła: nigdy nie wysyłaj wolniej/niepoprawnie niż committed;
`gemm_i8mma` zostaje fallbackiem). Committed kernel bez zmian (drzewo == HEAD).
Szczegóły + surowe liczby + plan Phase C: `docs/BENCH_COMPARISON.md`.

## W4A8 integracja in-tree (2026-07-19) — kernel + packer + launcher UDOWODNIONE w silniku; routing to pozostały krok

Phase C wykonana do warstwy kernela. W4A8 GEMM działa **wewnątrz FORGE** (cubin ładowany
tą samą ścieżką `cuModuleLoadData` co `gemm_i8mma`) i jest **zweryfikowany poprawnościowo
na realnym 4090** — standalone ORAZ przez HAL FORGE z packerem w Ruście. Jest
**non-default**: nic w silniku jeszcze do niego nie routuje, committed CUDA MMQ zostaje
ścieżką prefill Q4_K.

Zrobione i zweryfikowane (surowe liczby w `docs/BENCH_COMPARISON.md`):
- **Harness poprawności NAJPIERW** (`scratch/w4a8/harness.cu`): odtwarza dokładny 8-D
  interleave wag QServe + reorder scale/zero + per-token int8 act-quant w host C++, uruchamia
  `dense_kernel0` QServe i porównuje z niezależnym golden CPU int4×int8 (modelującym bytewise
  int8 wrap kernela). **relL2 ~2e-4 (szum fp16) na wszystkich kształtach** — layout byte-exact.
- **Kernel in-tree**: `kernels/cuda/w4a8_gemm.cu` (QServe `dense_kernel0`, MIT, 4 wejścia
  `extern "C"` per konfig CTA) → committed cubin `build/sm_89/w4a8_gemm_cuda.cubin` przez
  `build.sh`; wpisany w `registry.rs`; launcher `launchers.rs::w4a8_gemm` (grid/block/smem jak
  host QServe).
- **Requant/packer w Ruście**: `forge_formats::w4a8` (`w4a8_pack` / `w4a8_reconstruct`).
- **Poprawność w silniku przez HAL**: `crates/forge-kernels/tests/cuda_w4a8.rs` (packer Rust →
  HAL → cubin vs golden CPU) **PASS, relL2 ~2e-4** na wszystkich gałęziach CTA + kształtach
  FFN Mistral.
- **Perf w silniku**: ~420 TFLOP-eq @T≥2048 przez HAL = **2.0–2.1× llama.cpp (~206)**,
  bez narzutu HAL vs standalone.
- Gate 1 (build + clippy `--release --workspace`) zielony; Gate 4 (golden NVFP4 Bielik)
  **bit-exact 1 i 4 lanes** — NVFP4/Q8_0 nietknięte.

Pozostały krok (non-default): **routing prefill Q4_K → W4A8 w forward pass** (`FORGE_GEMM=w4a8`)
+ **requant-at-load** + **GPU per-token int8 act-quant**. Główna przeszkoda integracyjna:
**adresowanie okna wierszy** — FORGE trzyma q/k/v i gate/up **fused** i tnie je przez `row_off`
w `gemm_rows`, a bufory scale/zero QServe są w **transponowanym** `[K/G][N]`, więc okno wierszy
to niespójny wycinek kolumn przy pełnym stride N. Rozwiązania: (a) requant per-logiczna-macierz
na osobne zestawy buforów przy load (dispatch po `row_off`, bez okienkowania) lub (b) launcher
okienkowy z pełnym stride N + offset kolumny. Musi być bramkowane koherencją (3–4 prompty
faktograficzne, greedy) + perplexity proxy + koszt requant **~10.2% relL2** (Phase B) zanim
stanie się choćby opcją non-default. Projekcja bez zmian: sam W4A8 GEMM → pp4096 **~9650 =
0.80×** llama.cpp 12032; reszta luki to narzut nie-GEMM FORGE (~0.205 s vs 0.065 s) — cel fuzji
Phase 2. llama.cpp baseline nie odtworzony niezależnie w tej sesji (build `scratch/bench` skasowany);
użyto committed, potwierdzonego przez maintainera **12032**.

## W4A8 routing zintegrowany e2e (2026-07-19) — DZIAŁA, ale QUALITY-FAIL → zostaje non-default

Ostatni krok wykonany: W4A8 jest wpięty w prefill forward pass i mierzony e2e.
Committed CUDA MMQ pozostaje **domyślną** ścieżką Q4_K; W4A8 wybierany tylko przez
`FORGE_GEMM=w4a8`. Decode + Q8_0 + NVFP4 nietknięte.

**Jak rozwiązano bloker fused-weight:** wariant (a) — **requant per-logiczna-macierz
przy load**. Gdy `FORGE_GEMM=w4a8` i model jest dense, każda projekcja (q, k, v, o,
gate, up, down) jest osobno dequantowana Q4_K→f32 i pakowana `w4a8_pack` do WŁASNEGO
zestawu buforów (`weights.rs::pack_w4a8_weight`, `W4A8Layer`), więc transponowane
scale/zero `[K/G][N]` nie wymagają żadnego okienkowania. Wagi Q4_K zostają w VRAM dla
decode + głowy logitów (W4A8 to store DODATKOWY, ~+4 GiB). Routing: `model.rs`
prefill_chunk, `self.gemm_w4a8(...)` dla q/k/v/o/gate/up/down gdy `weights.w4a8`=Some.

**Kernel act-quant:** `forge_w4a8_quant_act_pertoken` (dodany do `w4a8_gemm.cu`,
zarejestrowany jako `w4a8_quant_act`) — per-token symetryczny int8 + skala f16,
1 blok/token, block-reduce amax. Launcher `Kernels::gemm_w4a8` robi quant→GEMM na
jednym stream. Grow-only scratch `W4A8ActScratch`.

**Gate'y (surowe liczby w `docs/BENCH_COMPARISON.md`):**
- Gate 1 build + `clippy --release --workspace`: zielone.
- Gate 2 NVFP4 Bielik golden: **bit-exact 1 i 4 lanes** (nietknięte).
- Gate 3 KOHERENCJA: **FAIL** — W4A8 produkuje bełkot na wszystkich 4 promptach
  ("Question: Question: …", "Qurbananas", "QGraphicsView::paintEvent"). Committed
  Q4_K spójny i faktograficzny. Przyczyna (oczekiwana): naiwny per-token int8
  act-quant BEZ SmoothQuant → outliery aktywacji rozwalają skalę per-token. Requant
  wagi Q4_K→W4A8 dokłada ~10% relL2, ale dominującym czynnikiem jest brak smoothingu
  aktywacji. NLL proxy: Q4_K mean NLL 3.53; strona W4A8 nieuchwycona (długi `serve`
  jest SIGURG-ubijany w tym harnessie) — sygnał jakościowy i tak jednoznaczny.
- Gate 4 PERF (4090 idle/cool, `-fa 1` llama.cpp potwierdzony: pp4096 **11955**):
  W4A8 pp4096 **5812** (1.52× nad committed 3825; **0.49×** llama.cpp), pp8192 **4109**
  (1.40×), pp512 **2937** (0.95×, mały kształt regresuje). Decode niezmieniony (±0.2%).
  In-engine GEMM: **161 → 39 ms/chunk = 4.1×** (lepiej niż 2× microbench), ALE e2e
  tylko 1.4–1.5× bo prefill jest teraz **non-GEMM-bound** (attention ~70% chunku).
  Projekcja ~9650 (0.80×) NIE ziściła się: zakładała że GEMM dominuje; realnie luka
  do llama.cpp to jego **fused flash-attention**, nie GEMM.

**Pozostała luka (żeby W4A8 był użyteczny):** offline per-channel activation
smoothing (SmoothQuant) przy pakowaniu + odpowiadająca skala w quantizerze; dopiero
wtedy koherencja może przejść. Osobno: fuzja attention/norm/quant (Phase-2) żeby e2e
prefill przekroczył ~0.5× llama.cpp. Committed default bez zmian.

---

## Przenośność na pre-Ada (RTX 3090 sm_86) + sprzątanie CUDA (2026-07-20)

Cel użytkownika: (a) usunąć martwy CUDA tam gdzie Mojo wystarcza, (b) uruchomić
FORGE na 3090. Oba załatwione strukturalnie — bez potrzeby kernela int8-multistage
(`fp8mod` już zdejmuje wyjątek ADR-0001 na Adzie, a na pre-Ada działa natywny int8/f16
Mojo). 3090 zweryfikowany przez `ptxas -arch=sm_86` (cross-assembly = dokładnie to, co
robi driver JIT przy ładowaniu); brak fizycznego sm_86 w tej maszynie.

**Dwa blokery (zmierzone):**
1. Wszystkie 273 committed PTX miały `.target sm_89` → JIT jest tylko w przód, więc
   NIE ładują się na sm_86. Żaden kernel Mojo nie wstałby na 3090.
2. Cztery cubiny nvcc (`gemm_i8mma`, `w4a8`, `fattn`, `mmq_q4k`) ładowane
   BEZWARUNKOWO przy starcie — sm_89 SASS bez fallbacku PTX → twardy crash na sm_86.

**Fix (scommitowany, gate'y zielone):**
- `build_kernels.mojo::_finalize` przepisuje `.target sm_89 → sm_80` dla każdego
  kernela poza `*fp8*`/`*nvfp4*`. Committed artefakty przetargetowane: **251/273** na
  `.target sm_80`, zweryfikowane `ptxas -arch=sm_86` ORAZ `-arch=sm_89` z **0 błędów**.
  22 zostają sm_89 (fp8 mma, fp8-KV cvt, NVFP4 fp8-scale cvt) — `ptxas -arch=sm_86` je
  ODRZUCA (sprzętowo Ada). Neutralne wydajnościowo na Adzie: driver re-JIT-uje sm_80 PTX
  do sm_89 SASS przy ładowaniu (Mistral pp4096 warm **11137 tok/s** = committed default;
  decode 149.9 bez zmian).
- `registry.rs` ładuje sm_89-target PTX i cubiny nvcc TYLKO przy `DeviceCaps.fp8_native`
  (sm ≥ 89) — `is_sm89_only()` skanuje nagłówek PTX. 3090 startuje nie dotykając żadnego
  niekompatybilnego modułu.
- **CUDA MMQ WYCOFANE (2026-07-20, Finding Q).** Domyślnym prefill GEMM dla Q4_K na
  WSZYSTKICH architekturach jest natywny Mojo int8 `gemm_q4k_i8_native` (czyta te SAME
  bajty `DevWeight::Q4K.buf` co dekodowy dp4a GEMV — prawdziwe 1× VRAM, bez repacku).
  Q6_K prefill → przenośny `gemm_q6_k_f16`. Cała domyślna ścieżka GGUF jest teraz 100 %
  Mojo PTX (`.target sm_80`), przenośna na dowolne sm_80+ (3090/4090) i przez codegen na
  AMDGPU/Metal.
- Usunięte: `kernels/cuda/mmq_q4k.cu`, `kernels/cuda/vendor/llama-cpp/`, `mmq_q4k_cuda.cubin`,
  ~200 wpisów `CUDA_MMQ_ENTRIES` + embed cubina + oba loadery, `MmqScratch`/`gemm_mmq`,
  cała rodzina `gemm_mmq_*`/`mmq_*`/`rmsnorm_q8_1_ds4` i fused `mmq_fuse`, `FORGE_GEMM=mmq`
  oraz krok nvcc MMQ w `build.sh`. Zero wiszących referencji. Nie-domyślne opt-iny Ada
  (`fp8mod`, `w4a8`, `fattn`) zostają jako sm_89 cubiny, strażowane per-arch.
- Toolchain: pełny `build_kernels.mojo` nie buduje się w obecnym środowisku (istniejące
  kernele fp8 mma emitują PTX ISA 8.1, którą lokalny ptxas odrzuca — wymaga 8.4; ich PTX
  są już w repo). 24 natywne PTX int8 regenerowane w izolacji przez `build_q4k_native.mojo`.

**Co musi zrobić user 3090:** nic ponad zwykły build — committed PTX ma już
`.target sm_80` i JIT-uje na 3090. NVFP4/fp8/W4A8 i CUDA-flash-attn zostają Ada-only
(sprzętowe fp8/fp4) i są przezroczyście pomijane; 3090 uruchamia modele GGUF na Mojo.

**Gate'y (po wycofaniu MMQ, 2026-07-20):** build+clippy zielone, zero wiszących referencji;
NVFP4 Bielik golden bit-exact 1 i 4 lanes (default nietknięty); Mistral Q4_K PPL
**30.3113** vs baseline MMQ **30.31** (bit-exact z konstrukcji); koherencja → „Paris,
France"; prefill warm **~5740 tok/s** vs ~11151 MMQ (**~0.51×, ~2× WOLNIEJ** — zmierzone niezależnie; wcześniejsze „11120/0.997×" było błędem pomiaru. Przyczyna: native Q4_K ~0.79×, Q6_K down-proj na wolnym f16 (był MMQ), utracona fuzja rmsnorm→q8_1 + narzut MPAD. Dług: przywrócić fuzję + szybki Q6_K Mojo + shared activation; próg
0.79×); decode **151 tok/s** bez regresji; VRAM wag 1× (bez repacku), scratch aktywacji
~+124 MB (skala MB, pomijalne przy KV-cache).

---

## Scalony DeltaNet prepare T=2-4 (2026-07-21)

- Dodano kernele Mojo `deltanet_prepare_t2_f16`, `t3` i `t4`, które jednym
  wywołaniem wykonują splot przyczynowy z SiLU, checkpointy okna, podział QKV,
  L2 i repeat Q/K oraz obliczenie `g` i `beta`. Stan wejściowy okna jest tylko
  odczytywany, więc odrzucony draft nie wymaga przywracania stanu.
- Golden CPU/GPU przeszedł dla T=2/3/4. PTX ma target `sm_80`, przechodzi
  `ptxas -arch=sm_80` bez spillów; warianty T2/T3/T4 używają odpowiednio
  64/68/78 rejestrów i 128 B pamięci współdzielonej.
- Izolowany pomiar RTX 4090 dla `n_k=16`, `n_v=32`, `d_state=128`, `d_conv=4`:
  fused T4 **0,00839 ms**, a 17 rozłożonych wywołań GPU bez kosztu kopii i repeat
  **0,02953 ms**, czyli konserwatywne przyspieszenie **3,52×**.
