# FORGE wobec llama.cpp na Radeonie AI PRO R9700 — Qwen3.6-27B (2026-07-30)

Stanowisko: jedna R9700 (`gfx1201`, RDNA4, 32 GiB), Ryzen 9 7950X. W maszynie są
dwie takie karty, ale **wszystkie pomiary poniżej są jednokartowe**
(`HIP_VISIBLE_DEVICES=0`) — druga karta służyła do kompilacji katalogu kerneli i
mikrobenchmarków, żeby nie zaburzać pomiaru.

Modele (oba to ten sam checkpoint `ThinkingCap-Qwen3.6-27B-MTP`, `qwen35`,
65 bloków, `nextn_predict_layers = 1`):

- `qwen36-27b-Q4_K_M.gguf` — 15,65 GiB, `token_embd` i większość projekcji Q4_K,
  `ffn_down` oraz `attn_qkv` w Q6_K,
- `qwen36-27b-NVFP4.gguf` — 16,95 GiB wg llama.cpp (plik 18,2 GB).

llama.cpp: build `3018a11e (109)`, `-DGGML_HIP=ON -DAMDGPU_TARGETS=gfx1201`,
`-ngl 99 -fa 1`. FORGE: `forge bench --prefix-cache off`.

## 1. Wynik — p1024 / tg128

`llama-bench -p 1024 -n 128` wobec `forge bench --prompt-tokens 1024 --tokens 128`.

| model | miara | FORGE przed | **FORGE teraz** | llama.cpp | |
|---|---|--:|--:|--:|---|
| Q4_K_M | prefill p1024 | 842,9 | **1481,2** | 1027,9 | **FORGE 1,44x** |
| Q4_K_M | decode tg128 | 27,7 | **27,7** | 27,4 | FORGE 1,01x |
| Q4_K_M | decode tg128 + MTP | *nie startowało* | **55,8** | brak w llama-bench | 2,01x nad własnym decode |
| NVFP4 | prefill p1024 | 1013,4 | **1311,2** | 929,0 | **FORGE 1,41x** |
| NVFP4 | decode tg128 | 29,1 | **29,1** | 28,0 | FORGE 1,04x |
| NVFP4 | decode tg128 + MTP | 66,0 | **66,0** | brak w llama-bench | 2,27x nad własnym decode |

`llama-bench` nie ma trybu spekulatywnego, więc MTP po obu stronach mierzy się
osobno, na realnym prompcie (§2).

Suma SHA 128 wygenerowanych tokenów jest **ta sama dla obu kwantyzacji, z
MTP i bez** (`0bf2b86b…`) — czyli ani optymalizacje prefillu, ani spekulacja nie
zmieniły ani jednego tokena.

## 2. Realny prompt — jedyny uczciwy pomiar MTP

Ten sam prompt po polsku (pytanie o HBM wobec GDDR), 200 tokenów, `temp 0`,
szablon czatu po obu stronach. `llama-cli -st` wobec `forge run`. Liczby FORGE to
sam decode (całość minus prefill 56 tokenów).

| model | tryb | FORGE | llama.cpp | |
|---|---|--:|--:|---|
| Q4_K_M | bez spekulacji | 27,3 | 27,3 | remis |
| Q4_K_M | MTP K=3 | **37,6** | 36,4 | **FORGE 1,03x** |
| NVFP4 | bez spekulacji | **28,6** | 27,9 | FORGE 1,03x |
| NVFP4 | MTP K=3 | 40,6 | **46,4** | llama.cpp 1,14x |

Syntetyczny prompt z ziarna tokenizera zawyża MTP po obu stronach (model
przewiduje sam siebie): stąd 66,0 tok/s w §1 wobec 40,6 tok/s tutaj.

**Została jedna przegrana komórka w całej tabeli: MTP na NVFP4.** Rozbicie
poniżej pokazuje, że nie jest to koszt cyklu, tylko akceptacja draftu.

## 3. Optymalizacja prefillu — co dało ile

Kolejność wyznaczył PROFIL (`rocprofv3 --kernel-trace`), nie intuicja. Rozkład
czasu prefillu Q4_K_M (p1024) w stanie wyjściowym:

| kernel | udział |
|---|--:|
| `gemm_q4_k_wmma` | 52,5% |
| **`gemm_q6_k_dot4`** | **25,4%** |
| `deltanet_prepare` | 11,8% |
| `deltanet_value_key` | 5,7% |
| `attn_prefill` | 3,4% |

### 3.1 Q6_K nie miał kernela macierzowego

Q2_K, Q3_K, Q4_K i Q5_K miały wariant WMMA; **Q6_K jako jedyny K-kwant liczył się
na `v_dot4_i32_i8`**. W tym checkpoincie w Q6_K są `ffn_down` i `attn_qkv`, czyli
jedna czwarta prefillu. Jedna projekcja `down` kosztowała 4,66 ms wobec 1,43 ms
projekcji Q4_K o tej samej liczbie mnożeń.

`src/gemm_q6_k_wmma.mojo` czyta surowe superbloki 210 B tak samo, jak rodzina
Q5_K czyta swoje 176 B. Kluczowa własność układu: szesnaście kolejnych kolumn ma
to samo `half`, tę samą grupę i to samo `l // 16`, więc **dzieli jedną skalę** i
leży w jednym ciągłym odczycie 16 bajtów `ql` oraz 16 bajtów `qh`. Kafel K=16
pokrywa się z granicą skalowania i skalę wolno wmnożyć w wagi przed mnożeniem
macierzowym, zamiast rozbijać akumulację.

Skutek uboczny, który jest tu ważniejszy niż sama prędkość: ścieżka `dot4`
kwantyzowała aktywacje do int8, WMMA liczy w f16. **Suma SHA 128 wygenerowanych
tokenów zmieniła się z `d6e51316…` na `0bf2b86b…` — czyli na dokładnie tę samą,
którą daje ten model w NVFP4.** Q4_K przestał się rozjeżdżać z referencją.

### 3.2 Szesnaście linii czytało fragment z jednego banku LDS

Wszystkie kafle tej rodziny (Q4_K, Q6_K, NVFP4 GGUF) trzymają rozpakowane wagi w
LDS z krokiem wiersza `CHUNK * 2 = 128 B`. Fragment `b` czyta linia `lane % 16`,
czyli szesnaście linii pod adresami odległymi o 128 B — a to **dokładna
wielokrotność 32 banków LDS**, więc wszystkie trafiają w ten sam bank.

Rozsunięcie wiersza o 16 wartości (`LDS_PAD`) kosztuje 4 KiB LDS na kafel i nie
zmienia matematyki. Zmierzone na R9700, T=1024, TFLOPS:

| kernel | kształt | bez rozsunięcia | z rozsunięciem |
|---|---|--:|--:|
| Q4_K | 6144x5120 | 65 | **73** |
| Q4_K | 5120x6144 | 49 | **65** |
| Q6_K | 5120x17408 (T=512) | 52 | **60** |
| NVFP4 | 6144x5120 | 51 | **68** |

### 3.3 Kafel BN=64 czytał aktywacje dwa razy za dużo

Ruch aktywacji to `(n_rows / BN) * T * K * 2 B`. Przy BN=64 macierz aktywacji
`ffn_down` (35,6 MB) jest czytana 80 razy — mieści się w 64 MB Infinity Cache, ale
i tak dominuje. BN=128 połowi ten ruch i **wygrywa na każdym zmierzonym
kształcie**, w każdym z trzech formatów (TFLOPS, T=1024):

| format | kształt | BM256/BN64 | BM256/BN128 | BM512/BN128 |
|---|---|--:|--:|--:|
| Q4_K | 17408x5120 | 70 | 82 | **93** |
| Q4_K | 6144x5120 | 68 | 89 | **100** |
| Q4_K | 5120x6144 | 50 | **87** | 85 |
| Q6_K | 10240x5120 | 60 | 74 | **92** |
| NVFP4 | 17408x5120 | 65 | 77 | **85** |
| NVFP4 | 6144x5120 | 64 | 82 | **99** |

BM=512 potrzebuje T >= 512, żeby mieć czym wypełnić kafel, więc wybór należy do
launchera, który zna długość chunka.

**Nazwa kafla musi nieść jego geometrię.** Obraz HSACO jest związany z
architekturą, a zestaw gfx1100 jest już zbudowany i ma pod `_bm256` kafel BN=64.
Gdyby nowa geometria weszła pod starą nazwą, launcher liczyłby siatkę z BN=128
dla kernela kafelkującego po 64 i po cichu pomijałby połowę wierszy. Stąd
`_bm256_bn128` i `_bm512_bn128` obok zachowanego `_bm256`.

### 3.4 Q6_K: przesunięcie o 32 ściąga się na liczbie całkowitej

Pierwsza wersja liczyła wagę jako `q6 * scale - 32 * scale` w f16, tak jak Q4_K
i Q5_K ściągają swój człon `dmin`. Odejmowanie przed skalowaniem jest dokładne
(kod ma sześć bitów) i zostawia jedno zaokrąglenie zamiast trzech, więc zostało —
ale **na tych danych nie zmieniło wyniku o ani jedną cyfrę**, więc nie jest to
poprawka wydajności ani dokładności, tylko tańszy zapis tej samej wartości.

Szukanie tego było skutkiem BŁĘDNIE SKALIBROWANEGO TESTU, i to jest tu lekcja.
`gemm_q6_k_matches_canonical_dequant_shapes` porównywał wynik GPU z referencją,
która kwantyzowała aktywacje do int8 — bo do tej pory Q6_K szedł na AMD kaflem
`dot4`, więc obie strony kwantyzowały tak samo. Po przejściu na WMMA referencja
opisywała już inną arytmetykę niż kernel. Po wyrównaniu jej do faktycznie
wybranego kafla zostało 3% błędu WZGLĘDNEGO w jednym elemencie na 16384 — i to
też nie jest usterka: zmierzone `want` wynosi tam 1,8e-3 przy członach iloczynu
rzędu 1, czyli iloczyn skalarny kasuje się prawie do zera, a zaokrąglenie wagi do
f16 (robi to CAŁA rodzina kafli WMMA) zostaje w wyniku jako kilka procent.
`golden.rs` ma na takie ścieżki próg 0,05 i ten test dostał ten sam.

Przy okazji usunięty został predykat `Kernels::int8_batch_activations()`: mówił
„na AMD batch kwantyzuje aktywacje", co po wejściu kafli WMMA przestało być
prawdą, a jedyne żywe użycie było właśnie w tej referencji.

### 3.5 Roofline karty — zmierzony, nie z papieru

Bez tych liczb nie da się powiedzieć, czy kernel jest szybki. Wszystkie zmierzone
na tej R9700 (`bench-amd/bench_wmma_gfx11.mojo`, `bench_roofline_gfx.mojo`):

| jednostka | przepustowość | wobec f16 |
|---|--:|--:|
| f16 WMMA 16x16x16 | 179 TFLOPS | 1,0x |
| int8 WMMA 16x16x16 | 357 TOPS | 2,0x |
| **fp8 WMMA 16x16x16** | **378 TFLOPS** | **2,1x** |
| **iu4 WMMA 16x16x32** | **743 TOPS** | **4,2x** |
| odczyt DRAM | 551 GB/s | |
| odczyt Infinity Cache (64 MiB) | 1828 GB/s | 3,3x DRAM |

`iu4` sprawdzony na przypadku ujemnym (−32 zamiast cichego 15 wariantu bez znaku).

### 3.6 Gdzie naprawdę jest sufit kafla f16

Kafel Q4_K liczy 73–104 TFLOPS zależnie od kształtu. Trzy pomiary rozstrzygają,
co go trzyma — i dwa pierwsze obalają hipotezy, które wyglądały oczywiście:

1. **Dekwantyzacja nie kosztuje nic.** Ten sam kafel na wagach JUŻ w f16 daje
   97 TFLOPS wobec 97–104 dla Q4_K. Rozpakowanie superbloku chowa się całkowicie
   za mnożeniami.
2. **To nie jest ruch globalny.** Fragment `a` czyta szesnaście wierszy odległych
   o `n_cols * 2` bajtów (10 KB), czyli zupełnie nieskoalescowanie — ale
   przepuszczenie aktywacji przez LDS jest DWA RAZY WOLNIEJSZE (35–67 TFLOPS).
3. **To ściana rejestrów.** Kafel ma 189 z 256 VGPR: 16 akumulatorów po 8 f32 to
   128, a każdy fragment f16 zajmuje 8. Każda zmiana, która potrzebuje więcej —
   potokowanie odczytu następnego podkroku (18 TFLOPS), MTILE=8 (17 TFLOPS) —
   kończy się zrzutem rejestrów. Sama instrukcja nie jest winna: mikrobenchmark
   z szesnastoma niezależnymi akumulatorami trzyma 180 TFLOPS.

**Wniosek: f16 nie da się poprawić kafelkowaniem, a formaty z blokową skalą nie
pomogą.** Przy skali zmieniającej się co 32 kolumny (Q4_K, Q6_K, NVFP4)
akumulator int32 trzeba zrzucić do f32 w środku pętli: dla `iu4` to około 26
cykli na kafel wobec 13,6 cyklu samej instrukcji, czyli mimo 4,2x szybszej
instrukcji rachunek wychodzi gorzej niż f16. Dlatego wcześniejsza próba
`gemm_q4_k_i8wmma` wyszła 3,3x wolniej — i dlatego kolejna próba w tę stronę też
by wyszła.

### 3.7 FP8 zdejmuje ścianę

`src/gemm_fp8_wmma.mojo` ma tę samą geometrię, ale fragment operandu zajmuje
**2 VGPR zamiast 8**, a skale są per wiersz wagi i per token — czyli stałe wzdłuż
K, więc w pętli wewnętrznej NIE MA żadnego zrzucania akumulatora. Zmierzone
(TFLOPS, T=1024):

| kształt | kafel f16 | **fp8 BM512/BN128** | **fp8 BM256/BN128** |
|---|--:|--:|--:|
| 17408x5120 (`ffn_gate`/`ffn_up`) | 97 | **203** | 172 |
| 5120x6144 (`ssm_out`) | 79 | 139 | **184** |

**2,1–2,3x na najgrubszym kernelu prefillu**, przy błędzie względnym 1,5e-5 wobec
referencji hosta (test złoty `tests_amd_fp8_wmma.mojo`, trzy kafle). To jest
pierwsza wersja, bez potokowania — na które teraz jest miejsce w rejestrach.

### 3.8 Wynik

| krok | Q4_K_M prefill | NVFP4 prefill |
|---|--:|--:|
| stan wyjściowy | 842,9 | 1013,4 |
| WMMA dla Q6_K | 975,8 | — |
| rozsunięcie LDS | 1008,9 | 1036,1 |
| kafel BN=128 | 1261,7 | 1282,2 |
| DeltaNet równolegle po tokenach + wąskie kafle | **1442,4** | 1283,8 |
| **razem** | **+71,1%** | **+26,7%** |
| **wobec llama.cpp** | **1,40x** | **1,38x** |

Suma SHA 128 wygenerowanych tokenów jest przez całą tę drogę ta sama
(`0bf2b86b…`) i identyczna dla obu kwantyzacji.

### 3.9 DeltaNet: 8,8x z samego kształtu siatki

`deltanet_prepare_dynamic_f16` miał JEDEN BLOK NA GŁOWĘ i przechodził wszystkie
tokeny w pętli — dla 27B to 64 bloki na karcie o 64 CU, czyli dwie fale na SIMD
przy 1024 iteracjach szeregowo. Tymczasem jedyna zależność między tokenami to
przyczynowy splot o oknie `d_conv - 1`, którego wejście już leży w pamięci.
Druga oś siatki (32 tokeny na blok) daje 3036 -> 346 us na warstwę, **bitowo ten
sam wynik** (0 rozbieżnych wartości), czyli 135 -> 17 ms w prefillu.

Tą samą drogą poszły wąskie projekcje: bramki `ssm_alpha`/`ssm_beta` mają 48
wierszy, a kafel prefillowy pokrywa 128, więc dostawały DWA bloki robocze na całą
kartę — 222 us na wywołanie, 96 wywołań, 21 ms zmarnowane. Mały kafel ma tam 32
bloki: 21 -> 8,9 ms.

## 4. MTP dla Q4_K_M — czego brakowało

llama.cpp wyciągał z MTP na tym pliku 1,33x, my nie startowaliśmy w ogóle. To
nie była luka wydajnościowa, tylko trzy brakujące kawałki — w Q4_K_M inne
tensory są w innych formatach niż w wariancie NVFP4:

| tensor | NVFP4 | Q4_K_M | co było potrzebne |
|---|---|---|---|
| `token_embd` | NVFP4 | **Q4_K** | gather Q4_K (wsadowy i jednowierszowy) |
| `nextn.eh_proj` | Q8_0 | **Q4_K** | przekwantowanie przy ładowaniu |
| `output` (głowa) | Q8_0 | **Q6_K** | batchowa głowa logitów z wyjściem f32 |

1. **Gather embeddingu.** 715 MB tablicy — przekwantowanie odpada, więc doszły
   `gather_q4_k_rows_f16` i `gather_q4_k_row_f16`, ta sama formuła co
   `gemm_q4_k_wmma`. Test złoty porównuje oba z kanoniczną dekwantyzacją
   `forge-formats` i sprawdza, że ID spoza zakresu daje wyzerowany wiersz.
2. **`eh_proj`.** Cała ścieżka MTP (`mtp_prepare_f16`, `mtp_project_joined_q8_f16`)
   czyta tę jedną macierz kernelem Q8_0. Przekwantowanie przy ładowaniu kosztuje
   26 MiB VRAM na całą głowę i jest tańsze niż drugi komplet kerneli o innej
   arytmetyce — stąd `MtpTensorLoader::matrix_q8`, używane wyłącznie dla
   `eh_proj`.
3. **Głowa logitów Q6_K.** Weryfikacja MTP potrzebuje logitów dla T tokenów
   naraz, a `logits_gemm` miał dla K-kwantów tylko przemiat per token — czyli
   odczyt CAŁEJ głowy (1,27 GiB) raz na token draftu. Kernel batchowy Q6_K
   istniał, ale wyłącznie z wyjściem f16; sampling weryfikatora wymaga f32.
   Wariant f32 powstał przez sparametryzowanie tego samego kernela typem zapisu
   (`OUT: DType`, wzorzec z `gemm_dot.mojo`), więc matematyka i odczyt wag są te
   same — test złoty wiąże go wprost z wariantem f16.

## 5. Co dalej — z pomiarem, nie z intuicji

**Decode jest domknięty sprzętowo.** 16,8 GB wag na token w 33,6 ms to 500 GB/s
z osiągalnych 551 — **91% roofline'u DRAM**. Nie ma tam czego kafelkować: jedyna
dźwignia to CZYTAĆ MNIEJ BAJTÓW NA TOKEN, czyli akceptacja spekulacji.

**Prefill po tych zmianach** (per przebieg, Q4_K_M, T=1024):

| kernel | ms | udział |
|---|--:|--:|
| `gemm_q4_k_wmma` | 438,7 | 62% |
| `gemm_q6_k_wmma` | 101,2 | 14% |
| `deltanet_value_key` (skan) | 68,0 | 9,6% |
| `attn_prefill` | 41,0 | 5,8% |
| `deltanet_prepare` | 17,1 | 2,4% |
| reszta | ~34 | 5% |

Kolejność prac, jaką wyznaczają te liczby:

1. **FP8 strumieniowo — ZMIERZONE I ODRZUCONE.** Ścieżka powstała w całości
   (paker NVFP4->e4m3, dwa gniazda, drugi strumień, zdarzenia `packed`/
   `consumed`) i dała **970,0 tok/s wobec 1311,2** na NVFP4 p1024, czyli
   regresję o 26%. Projekcja „0,76 ms przepakowania chowa się za 5,0 ms GEMM-u"
   była błędna z dwóch powodów naraz:

   - **Przepakowanie robi tę samą pracę co GEMM, tylko dwa razy.** Paker
     dekwantyzuje każdą wagę w przebiegu absmax i drugi raz przy kodowaniu, a
     GEMM rozpakowywał ją raz. Dla `17408 x 5120` to 267 M wartości na projekcję
     i 3 projekcje na warstwę.
   - **Drugi strumień niczego nie chowa, bo obie prace są PRZEPUSTOWOŚCIOWE.**
     Nakładanie ukrywa opóźnienie, nie zajętość. Nie ma wolnych jednostek, w
     których cień mógłby się zmieścić, więc czas się sumuje: +345 ms
     przepakowania wobec ~70 ms oszczędności na GEMM-ach.

   Sedno jest strukturalne: **każda paczka służy dokładnie jednemu GEMM-owi**,
   więc jej koszt nigdy się nie amortyzuje. FP8 wygrywa TYLKO jako rezydencja,
   gdzie pakuje się raz przy ładowaniu i używa w każdym prefillu — a wtedy
   ogranicza je VRAM (pełna kopia e4m3 FFN to 17,4 GB przy modelu 17 GB na
   karcie 32 GiB). Realny wariant to rezydencja CZĘŚCIOWA: tyle warstw, ile
   mieści się w zapasie, bez kosztu przepakowania po rozgrzaniu.

   Z tej pracy ZOSTAJE `pack_nvfp4_gguf_fp8`: paker czyta surowe bloki 36 B GGUF
   NVFP4 i wkłada `output_scale` tensora w skalę wierszową paczki (GEMM e4m3 nie
   ma mnożnika wyniku). Jest bramkowany złotym testem
   `pack_gguf_fp8_matches_cpu_pack` w pięciu wariantach, w tym NVFP4 z
   mnożnikiem 0,0625. To on jest warunkiem wariantu rezydentnego.

2. **Uwaga prefillu na kaflu macierzowym — ZROBIONE.** `head_dim` jest
   parametrem kompilacji (`attn_prefill_wmma_impl[HD]`, instancje 128 i 256), a
   brakującym ogniwem był kontrakt pozycji bazowej: prefill layer-major podaje
   ją jako BUFOR GPU, a kafel WMMA brał SKALAR HOSTA. Wariant
   `attn_prefill_wmma_pos_hd256` czyta `base_pos[0]` i wchodzi w launcherze
   `attn_prefill_device_pos_f16_hd256`; bez artefaktu zostaje ścieżka skalarna,
   więc pozostałe karty nic nie tracą.

   | model | przed | po |
   |---|--:|--:|
   | Q4_K_M p1024 | 1446,7 | **1481,2** |
   | NVFP4 p1024 | 1295,9 | **1311,2** |

   Suma SHA bez zmian — kafel jest bitowo zgodny ze ścieżką skalarną.

3. **`deltanet_value_key`** — sprawdzona i ODRZUCONA tania hipoteza: skan ma na
   token dwie redukcje warpowe spięte zależnością `predicted -> delta -> state`,
   więc wyglądał na ograniczony opóźnieniem. Cztery kolumny na falę zamiast
   dwóch (cztery przeplatające się łańcuchy zamiast dwóch, wynik bitowo ten sam)
   dały **1475,2 tok/s wobec 1481,2** — czyli nic, w granicach szumu. Skan nie
   stoi na ILP redukcji. Zostaje chunkowa postać macierzowa (chunked linear
   attention), która zamienia go na GEMM-y — duża zmiana algorytmiczna.

4. **Akceptacja draftu MTP na NVFP4** — jedyna przegrana komórka w tabeli.
   Zmierzone, że to NIE jest koszt cyklu: target batchuje T=4 poprawnie (7,4 s
   GPU bez spekulacji wobec 3,7 s z MTP na te same 256 tokenów), a na
   syntetycznym prompcie przyspieszenie zgadza się z tym stosunkiem. Sprawdzone i
   ODRZUCONE dwie gotowe dźwignie: tańsza głowa draftu
   (`FORGE_MTP_DRAFT_HEAD=nvfp4`) podnosi akceptację 1,35 -> 1,44/krok, ale
   przebieg jest wolniejszy (5,78 s wobec 4,97 s), a K=2 wobec K=3 to remis
   (4,93 wobec 4,96 s). Zostaje jakość samego proposera — to wymaga porównania
   propozycji token po tokenie z llama.cpp, nie kolejnego pomiaru przepustowości.
5. Rozsunięcie LDS i kafel BN=128 wpisano do Q4_K, Q6_K i NVFP4. **Q2_K, Q3_K i
   Q5_K mają ten sam defekt** i czekają na własny pomiar — ten checkpoint ich nie
   używa.
6. `test_catalog_matches_committed_manifest` jest CZERWONY dla `gfx1100` i
   `gfx1030` i był czerwony już przed tą pracą (`gemm_q8_0_wmma_128x128` z
   poprzedniej sesji). Przyczyna jest strukturalna: HSACO jest związany z
   architekturą, więc katalog danej karty da się zbudować TYLKO na niej, a 7900 XT
   i 6900 XT nie ma już w maszynie. Kernele przenośne dodane w tej pracy
   (`gather_q4_k_*`, `deltanet_prepare_tokens_f16_t32`,
   `gemv_q6_k_dp4a_batch_out_f32_*`) należą do wszystkich zestawów i trafią tam
   przy najbliższym buildzie na tamtych kartach. gfx1201 przechodzi.
