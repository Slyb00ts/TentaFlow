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

## 1. Stan wyjściowy — punkt odniesienia

`llama-bench -p 1024 -n 128` wobec `forge bench --prompt-tokens 1024 --tokens 128`.

| model | miara | FORGE | llama.cpp | |
|---|---|--:|--:|---|
| Q4_K_M | prefill p1024 | 842,9 tok/s | **1027,9** tok/s | llama.cpp 1,22x |
| Q4_K_M | decode tg128 | **27,7** tok/s | 27,4 tok/s | FORGE 1,01x |
| Q4_K_M | decode tg128 + MTP | **nieobsługiwane** | — | patrz §4 |
| NVFP4 | prefill p1024 | **1013,4** tok/s | 929,0 tok/s | FORGE 1,09x |
| NVFP4 | decode tg128 | **29,1** tok/s | 28,0 tok/s | FORGE 1,04x |
| NVFP4 | decode tg128 + MTP | **66,0** tok/s | brak w llama-bench | 2,27x nad własnym decode |

`llama-bench` nie ma trybu spekulatywnego, więc MTP po obu stronach mierzy się
osobno, na realnym prompcie (§2).

## 2. Realny prompt — jedyny uczciwy pomiar MTP

Ten sam prompt po polsku (pytanie o HBM wobec GDDR), 200 tokenów, `temp 0`,
szablon czatu po obu stronach. `llama-cli -st` wobec `forge run`. Liczby FORGE to
sam decode (całość minus prefill 56 tokenów).

| model | tryb | FORGE | llama.cpp | |
|---|---|--:|--:|---|
| Q4_K_M | bez spekulacji | 26,6 tok/s | **27,3** tok/s | llama.cpp 1,03x |
| Q4_K_M | MTP K=3 | **nieobsługiwane** | **36,4** tok/s | luka |
| NVFP4 | bez spekulacji | **28,2** tok/s | 27,9 tok/s | FORGE 1,01x |
| NVFP4 | MTP K=3 | 39,2 tok/s | **45,3** tok/s | llama.cpp 1,16x |
| NVFP4 | zysk z samego MTP | 1,39x | **1,62x** | |

Wniosek jest ten sam co na RDNA3: **bazowy decode mamy na równi albo minimalnie
lepszy, a przegrywamy na maszynerii spekulacji.** Nasza akceptacja to 1,35
zaakceptowanego tokena na krok (2,35x tokenów na forward) — to jest miejsce do
poprawy, nie same kernele dekodowania.

Syntetyczny prompt z ziarna tokenizera zawyża MTP po obu stronach (model
przewiduje sam siebie): stąd 66,0 tok/s w §1 wobec 39,2 tok/s tutaj.

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

### 3.5 Wynik

| krok | Q4_K_M prefill | NVFP4 prefill |
|---|--:|--:|
| stan wyjściowy | 842,9 | 1013,4 |
| WMMA dla Q6_K | 975,8 | — |
| rozsunięcie LDS | 1008,9 | 1036,1 |
| kafel BN=128 | **1261,7** | **1282,2** |
| **razem** | **+49,7%** | **+26,5%** |
| **wobec llama.cpp** | **1,23x** | **1,38x** |

Suma SHA 128 wygenerowanych tokenów jest przez całą tę drogę ta sama
(`0bf2b86b…`) i identyczna dla obu kwantyzacji.

## 4. Czego jeszcze brakuje

1. **MTP dla Q4_K_M w ogóle nie wstaje.** `mtp_device_bytes` przyjmuje wyłącznie
   `Q8_0` i `NvFp4Gguf`, a w tym pliku `token_embd` ORAZ `nextn.eh_proj` są w
   Q4_K. llama.cpp wyciąga z MTP na tym samym pliku 1,33x, my zero — to jest
   największa pojedyncza luka w tabeli i nie jest to luka wydajnościowa, tylko
   brakujący gather i brakująca projekcja.
2. **Akceptacja draftu MTP.** 1,35 tokena na krok wobec zysku 1,62x u llama.cpp
   na tym samym pliku NVFP4.
3. **Kafle WMMA stoją na ~55% szczytu karty** (100 TFLOPS wobec zmierzonych 179
   TFLOPS f16 WMMA; int8 WMMA ma 354 TOPS). Jednostka int8 nie jest tu prostym
   zyskiem: skala Q4_K/Q6_K zmienia się co 32 kolumny, więc akumulator int32
   trzeba zrzucać do f32 co dwie instrukcje, a to zjada przewagę — dlatego
   wcześniejsza próba `gemm_q4_k_i8wmma` wyszła 3,3x wolniej. Realna droga to
   `v_wmma_i32_16x16x32_iu4`: K=32 to dokładnie jeden podblok skali, czyli jedno
   zrzucenie na instrukcję.
4. Po zamianie kafla Q6_K na WMMA rozkład prefillu Q4_K to `gemm_q4_k_wmma`
   60,9%, `deltanet_prepare` 14,3%, `gemm_q6_k_wmma` 12,5%, `deltanet_value_key`
   6,8%, uwaga 4,2%. **DeltaNet jest teraz drugim kosztem** i nie był jeszcze
   dotykany na tej karcie.
5. Rozsunięcie LDS i kafel BN=128 wpisano do Q4_K, Q6_K i NVFP4. **Q2_K, Q3_K i
   Q5_K mają ten sam defekt** i czekają na własny pomiar — ten checkpoint ich nie
   używa.
6. `test_catalog_matches_committed_manifest` jest CZERWONY dla `gfx1100` i był
   czerwony już przed tą pracą (`gemm_q8_0_wmma_128x128` z poprzedniej sesji):
   zestaw 7900 XT wymaga przebudowania na tej karcie, a jej nie ma w maszynie.
   gfx1201 przechodzi.
