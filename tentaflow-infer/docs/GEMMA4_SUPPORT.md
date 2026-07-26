# Obsługa Gemma 4 w FORGE — stan i plan

Model odniesienia: `google/gemma-4-12B-it-qat-q4_0-gguf`
(`gemma-4-12b-it-qat-q4_0.gguf`, 6,98 GB, 667 tensorów, 48 warstw).
Wszystkie liczby poniżej są ODCZYTANE z tego pliku, nie z dokumentacji.

## Architektura, którą trzeba obsłużyć

| parametr | wartość |
|---|---|
| `block_count` | 48 |
| `embedding_length` | 3840 |
| `feed_forward_length` | 15360 |
| `attention.head_count` | 16 |
| `attention.head_count_kv` | **[8, 8, 8, 8, 8, 1]** (wzorzec powtarzalny) |
| `attention.key_length` / `_swa` | **512** / **256** |
| `attention.sliding_window` | 1024 |
| `attention.sliding_window_pattern` | **[T, T, T, T, T, F]** (powtarzalny) |
| `rope.freq_base` / `_swa` | 1e6 / 1e4 |
| `rope.dimension_count` / `_swa` | 512 / 256 |
| `final_logit_softcapping` | 30,0 |
| `context_length` | 262144 |
| słownik | 262144, embedding Q6_K, wagi Q4_0 |
| `tokenizer.ggml.model` | `gemma4` |

Rozkład warstw wynikający z wzorca: warstwy o indeksie `% 6 != 5` są LOKALNE
(okno 1024, head_dim 256, 8 głowic KV), a `% 6 == 5` GLOBALNE (head_dim 512,
1 głowica KV). Potwierdzone na tensorach: `blk.0.attn_q` ma 4096 = 16 x 256,
a `blk.5.attn_q` 8192 = 16 x 512.

**Warstwy globalne nie mają projekcji V — wtedy V = K.** Potwierdzone w
implementacji wzorcowej (`llama.cpp` `ff067f76`, `src/models/gemma4.cpp`):

```cpp
// note: use_alternative_attention (v_proj is optional, if it's not present, use k_proj)
layer.wv = create_tensor(tn(LLM_TENSOR_ATTN_V, "weight", i), {...}, TENSOR_NOT_REQUIRED);
...
ggml_tensor * Vcur = model.layers[il].wv ? build_lora_mm(...) : Kcur;
```

Dodatkowo każda warstwa ma dwie normy „sandwich" (`post_attention_norm`,
`post_ffw_norm`) i skalar `layer_output_scale`.

## Referencja i baseline

`llama.cpp` z master (`ff067f76`) zbudowany pod ROCm/gfx1030 w
`~/llama.cpp-master/build-rocm-master` — OSOBNY worktree i katalog builda, żeby
nie ruszać buildu `~/llama.cpp/build-rocm`, z którego pochodzą dotychczasowe
liczby odniesienia dla qwena i Mistrala.

Zmierzone na tym modelu (6900 XT, `-ngl 99`, pp1024/tg128):

| silnik | prefill | decode |
|---|--:|--:|
| llama.cpp `ff067f76` | **1 384,7 tok/s** | **27,4 tok/s** |
| FORGE | nie uruchamia (patrz plan) | — |

Wyjście referencyjne (`--temp 0`, prompt „Wymień trzy największe miasta w
Polsce.”) jest sensowne i po polsku, z widocznym tokiem rozumowania modelu.
UWAGA: decode 27,4 zmierzone przy zegarze pamięci zatrzaśniętym na 456 MHz
(patrz `STATUS.md`), więc porównywać wolno tylko z naszym pomiarem w tym samym
stanie.

## Graf forward — ustalenia z implementacji wzorcowej

Rzeczy, których NIE DA SIĘ odgadnąć z samych metadanych, a każda zmienia wynik:

1. **`f_attention_scale = 1.0`** — Gemma 4 NIE skaluje przez `1/sqrt(head_dim)`.
2. **Embedding wejściowy mnożony przez `sqrt(n_embd)`** (tylko dla wejścia
   tokenowego, nie dla surowych embeddingów obrazu).
3. **V dostaje CZYSTĄ normalizację RMS bez wagi** (`ggml_rms_norm`), podczas gdy
   Q i K mają swoje wagi `attn_q_norm` / `attn_k_norm`.
4. Kolejność w bloku uwagi: projekcja -> reshape -> RMSNorm -> rope. K jest
   ropowane, V nie.
5. **Warstwy globalne używają tensora `rope_freqs`** jako `freq_factors`
   (proporcjonalne rope); warstwy z oknem nie.
6. FFN to **GeGLU z `ggml_gelu`**, czyli przybliżenie tanh — nie dokładny erf.
   Nasz `gelu_mul_f16` jest zgodny.
7. **`layer_output_scale` mnoży wyjście CAŁEJ warstwy**, po FFN i rezydualu.
8. Softcapping logitów: `tanh(x / cap) * cap` po `output_norm`, przed samplingiem.
9. Cache KV jest **przeplatany** (`build_attn_inp_kv_iswa`) — osobny dla warstw z
   oknem i osobny dla globalnych.

## Stan: działa, zweryfikowane względem llama.cpp

`google/gemma-4-12B-it-qat-q4_0-gguf` generuje spójny polski tekst na Radeonie
RX 6900 XT (gfx1030). Poprawność sprawdzono zrzutem tensorów z
`llama-eval-callback` (CPU, f32) i porównaniem sum oraz pojedynczych wartości na
każdym etapie: `attn_norm`, projekcje Q/K/V, normy Q/K/V, rope (obie podstawy i
`rope_freqs`), wyjście uwagi, projekcja wyjścia, normy sandwich, GeGLU, `down`,
strumień rezydualny po każdej z 48 warstw oraz logity. Rozjazd rośnie z długością
kontekstu i mieści się w szumie f16: 0,85% sumy logitów przy 11 tokenach i 2,4%
przy 176 (referencja liczy w f32 na CPU).

### Znalezione i naprawione błędy

1. `qk_norm_over_hidden` porównywało długość `attn_q_norm` z GLOBALNYM
   `head_dim`. Przy naprzemiennej geometrii warstwa 0 ma 256, a model raportuje
   512, więc norma leciała po całej projekcji 4096 wagą 256-elementową i czytała
   poza bufor. Porównanie używa teraz `head_dim` warstwy 0.
2. Prefill i dekodowanie wołały uwagę z globalnymi `head_dim`/`n_kv_heads` dla
   wszystkich warstw, co psuło mapowanie głowic GQA i offsety scalonego QKV.
   Obie pętle liczą wymiary per warstwa.
3. `gelu_mul_f16` liczyło `tanh` jako `(e-1)/(e+1)` z `e = exp(2x)`. Przy
   bramkach rzędu 30 (realnych w FFN) `exp` przelewało się do `inf`, dając
   `inf/inf = NaN`; ścieżka kwantyzacji aktywacji do int8 zamieniała te NaN w
   ciche śmieci, więc błąd nie propagował się jako NaN. Postać
   `1 - 2/(exp(2x)+1)` nasyca się poprawnie.
4. Ścieżka rozdzielna dekodowania nie nakładała norm sandwich ani
   `layer_output_scale` — stan rezydualny rozjeżdżał się od drugiej warstwy.
5. Cache KV był rozmiarowany jedną geometrią (512 elementów na token), a warstwy
   okienne potrzebują 2048. Rozmiar liczy teraz najszersza warstwa
   (`kv_cache_heads`/`kv_cache_head_dim`, jedno źródło prawdy dla puli i cache).
6. Brakowało maski `tokenizer.ggml.suppress_tokens`. Checkpoint przypisuje
   wysokie logity tokenom `<image|>`/`<audio|>` i bez maski greedy wybierał je
   jako pierwszy token (llama.cpp wstawia tę maskę jako wejście grafu).

### Wydajność (RX 6900 XT, gfx1030)

| pomiar | llama.cpp ff067f76 (ROCm) | FORGE | luka |
|---|---:|---:|---:|
| prefill pp1024 | 1449,1 tok/s | 1287,9 tok/s | -11,1% |
| decode tg128 (ctx 1024) | 52,4 tok/s | 38,5 tok/s | -26,5% |

Cel „prefill znacznie powyżej llama.cpp, decode co najmniej na równi" NIE jest
osiągnięty. Poniżej jest zmierzone rozliczenie czasu i to, co realnie stoi na
drodze — bez tego dalsza optymalizacja jest zgadywaniem.

#### Gdzie idzie prefill (T=1024, `FORGE_PREFILL_TRACE=1`)

| faza | ms | udział |
|---|---:|---:|
| gate/up | 309,3 | 39,2% |
| down | 182,2 | 23,1% |
| uwaga | 110,7 | 14,0% |
| qkv | 105,9 | 13,4% |
| projekcja o | 52,2 | 6,6% |
| pozostałe (normy, rope, kv_append) | 29,3 | 3,7% |

#### Dlaczego GEMM stoi na 41 TOPS z 92 TOPS `v_dot4_i32_i8`

Zliczenie ISA (`llvm-objdump` na `gemm_q4_0_dot4_128x128.hsaco`): **512
`v_dot4_i32_i8` na 1133 instrukcje wektorowe**, czyli 45% szczeliny wydania —
dokładnie tyle, ile wychodzi z pomiaru. Rozkład reszty: 192 operacje epilogu
(`cvt` + `mul` + `fma` na wyjście na każdy 32-kolumnowy blok skali Q4_0), 48
`ds_read_b128`, około 150 na adresowanie, `v_mov` i maski `exec`.

Epilog jest **strukturalny**: Q4_0 ma jedną skalę na 32 wartości, a skala wagi i
aktywacji różnią się per blok, więc trzeba go zastosować co 8 `dot4` na wyjście.
To daje 3/8 = 37,5% narzutu niezależnie od rozmiaru kafla, czyli sufit ~63 TOPS.
llama.cpp osiąga na tym samym pułapie około 46 TOPS (73% sufitu), my 41 (65%).

#### Co sprawdzone i odrzucone pomiarem

| hipoteza | wynik |
|---|---|
| potokowanie odczytów przez rejestry (podwójny bufor LDS) | +7-18% na wąskim N, -2% na gate/up; razem ~1,6% prefillu — usunięte, nie warte drugiej implementacji |
| większe kafle rejestrowe (128x128 TN8, 256x128, 128x256) | 2,2-2,7x gorsze (spadek zajętości) |
| usunięcie warunków zakresu w czasie kompilacji | bez zmian |
| `v_dot2_f32_f16` w iloczynie Q·K uwagi | +1,1% prefillu; uwaga 112,4 -> 110,7 ms, czyli QK jest ograniczony LDS, nie operacjami wektorowymi |
| rozwinięcie pętli pozycji w uwadze decode (4 pozycje) | 0% przy ctx 1024 i **-6,8% przy ctx 2048** (64 dodatkowe VGPR-y zabijają zajętość) — cofnięte |

Zmiana `dot2` została zachowana (jedyna z dodatnim wynikiem), reszta cofnięta.

#### Rozliczenie decode

Skalowanie z długością kontekstu (prompt 128/1024/2048): 49,7 / 38,5 / 36,9 tok/s.
Z różnicy między ctx 128 i 1024 wychodzi koszt brzegowy 5,9 ms na token za około
309 MB dodatkowych odczytów KV, czyli **52 GB/s**. Same wagi idą przy tym z
**362 GB/s** (6,96 GB w 19,2 ms), co jest blisko 358 GB/s liczonych dla
llama.cpp. Wniosek: ścieżka GEMV wag jest w porządku, a cały dystans na decode
siedzi w odczytach cache KV przy jednej sekwencji. Rozwinięcie pętli tego nie
naprawiło, więc przyczyna nie jest samą latencją; następny krok wymaga
profilera (`rocprofiler` nie jest zainstalowany, potrzebny root).

### Znane braki poza ścieżką Gemmy

`gemm_q6_k_f16` na gfx1030 NIE jest zepsuty — sprawdzone. Wcześniejszy błąd do
46% brał się z referencji testu: ścieżka AMD kwantyzuje aktywacje do int8 (q8_1,
skala na 32 kolumny), a referencja mnożyła wagi przez aktywacje f32. Przy
syntetycznych wagach Q6_K o dużej amplitudzie (`sc` do ±127) i kasowaniu
składników daje to dziesiątki procent. Z referencją świadomą tej kwantyzacji
kernel ma błąd 5e-4 na wszystkich sprawdzonych kształtach, a `Kernels`
udostępnia `int8_batch_activations()`, żeby testy wybierały właściwy kontrakt.

22 testy golden GEMM okazały się wcześniej po cichu pomijane na AMD (otwierały
wprost urządzenie CUDA i raportowały „ok" bez pokrycia). Teraz `golden.rs` używa
wspólnego selektora backendu, kernele nieskompilowane dla danej architektury są
jawnie raportowane jako pominięte, a cały zestaw kerneli to **161 testów, 0
błędów** na gfx1030 przy progu 2%.

## Poprzednie notatki

## Co jest już zrobione

- **Kafel `gemm_q4_0_dot4`** (int8 na `v_dot4_i32_i8`, warianty 64x64/128x64/128x128
  i out_f32). Sprawdzony wobec referencji hosta na trzech kształtach, błąd 4,8e-4
  czyli samo zaokrąglenie f16. Przydaje się każdemu modelowi Q4_0, nie tylko Gemmie.
- **`gelu_mul_f16`** — GeGLU z przybliżeniem tanh (wariant z referencji Gemmy,
  nie dokładny erf).
- **`gemma4.ron`** — mapa ról tensorów, z `AttnV` jako opcjonalną.
- **Nowe role**: `PostAttnNorm`, `PostFfwNorm`, `LayerOutputScale`.
- **`AltAttnParams`** w `Hyperparams` — naprzemienna geometria uwagi (wzorzec
  okien, `head_dim_swa`, `n_kv_heads_swa`, `rope_theta_swa`), plus
  `ffn_activation` i `final_logit_softcap`. Wzorce z GGUF są krótkie i
  powtarzalne, więc parser rozwija je modulo długość — bez tego geometria od
  siódmej warstwy byłaby zła.

## Postęp

- ✅ **Kernele uwagi head_dim 512.** `head_dim` był już parametrem kompilacji, więc
  decode skompilował się od razu. Prefill nie: kafle K i V przy `PT = WARP_SIZE`
  zajmowały **81 920 B LDS wobec limitu 65 536**. `PT` (pozycje na kafel) jest
  teraz parametrem i hd512 używa połowy fali — LDS 49 152 B, 122 VGPR, bez
  spillów. PUŁAPKA: przy `PT` mniejszym od fali lane'y powyżej `PT` czytały LDS
  poza `ks`; maskowanie samego wyniku było za późno, odczyt musi być warunkowy.
- ✅ **Okno przesuwne** w prefillu (maskowanie) i decode (przesunięcie startu
  skanu, więc mniej pracy zamiast więcej). PUŁAPKA: przy oknie kafel może mieć
  `h > 0` i być CAŁY poza oknem — wtedy wszystkie lane'y mają `NEG_INF`,
  maksimum nie rośnie ponad wartość startową i `exp(score - m)` daje 1,0
  zamiast 0, czyli softmax dostaje masę z nieistniejących pozycji. Taki kafel
  jest jawnie pomijany.
  Zweryfikowane wobec referencji hosta (softmax w f32, przyczynowy, stronicowany
  cache): hd256 bez okna 4,5e-5, hd512 5,8e-5 i 3,4e-5, z oknem 8/20/33 —
  wszystkie 3,4e-5..4,5e-5. Pierwszy wynik potwierdza brak regresji na
  hd64/128/256, które dzielą ten kod.
- ✅ Okno przeprowadzone przez launchery i 7 miejsc wywołania w silniku
  (`Model::attn_window(layer)` czyta rozwinięty wzorzec z `AltAttnParams`).
  Wariant `split8` nie ma jeszcze maskowania, więc przy oknie schodzimy na
  ścieżkę generyczną.

- ✅ **Tokenizer `gemma4`.** To BPE z JAWNĄ tablicą merge'ów (514 906 pozycji),
  ale w kształcie SPM: spacje zamienia normalizator na `▁`, pre-tokenizacja
  dzieli WYŁĄCZNIE po nowych liniach (`[^\n]+|[\n]+`), a tekst jest surowym
  UTF-8 — BEZ kodowania bajtowego GPT-2. `add_space_prefix` jest w tym modelu
  wyłączone, więc nie doklejamy `▁` na początku. Wszystkie cztery rzeczy różnią
  go od ścieżki `gpt2` i każda zmienia identyfikatory tokenów.
- ✅ **Walidacja wag per warstwa** plus wariant `V = K`. `Hyperparams` ma teraz
  `head_dim_at(layer)`, `n_kv_heads_at(layer)` i `has_v_proj(layer)`; pola
  skalarne opisują warstwy globalne, więc pytanie o konkretną warstwę zawsze
  idzie przez akcesor.

Model przechodzi dziś **ładowanie, walidację kształtów i budowę tokenizera**, a
zatrzymuje się dopiero w przebiegu forward:
`kernel arg offset 18800640 exceeds buffer size 17694720` — czyli offsety w
grafie liczone są nadal z jednolitej geometrii.

## Zakres cache'u KV — po rozpoznaniu mniejszy, niż zakładałem

`KvCache` trzyma `k: Vec<DevBuffer>` i `v: Vec<DevBuffer>` — **osobny slab na
warstwę**, a pula stron współdzieli tylko IDENTYFIKATORY stron. Ten sam
identyfikator może więc wskazywać w każdej warstwie na obszar o innym rozmiarze,
o ile indeksowanie używa stride'u tej warstwy. Założenie jednolitej geometrii
siedzi tylko w 13 miejscach: 7 w `kv.rs` (liczenie rozmiaru slabu) i 6 w
`model.rs` (arytmetyka bajtów przy tieringu, MTP, jedno uruchomienie).
Zmiana sprowadza się do `KvConfig` z geometrią per warstwa i akcesorów
`n_kv_heads(layer)` / `head_dim(layer)` — to godziny, nie tygodnie.

## Co zostaje, w kolejności

1. ~~Referencja dla warstw bez V.~~ **ZROBIONE** — V = K, patrz wyżej.
2. ~~Kernele uwagi head_dim 512.~~ **ZROBIONE**, patrz wyżej. Dawniej: Obecne specjalizacje to hd64/hd128/hd256, a
   silnik odrzuca inne wprost (`head_dim {} has no attention specialization`).
   Potrzebne decode i prefill, prawdopodobnie w wariancie split (hd512 z f16 KV
   to 2 KB na token na głowicę).
3. ~~Okno przesuwne~~ **ZROBIONE** (maskowanie; polityka cache'u zostaje). Dawniej: maskowanie w kernelach uwagi plus polityka w cache KV
   (warstwy lokalne trzymają najwyżej 1024 tokeny).
4. **Cache KV o geometrii per warstwa.** To najgłębsza zmiana: dziś układ zakłada
   jednolitą liczbę bajtów na token na warstwę, a tu jest 8 x 256 wobec 1 x 512.
   Istniejąca zwarta mapa `global_layer -> kv_layer` (z prac nad Qwen3.6) jest
   punktem wyjścia.
5. **Graf forward**: dwie normy sandwich, `layer_output_scale`, GeGLU, dwie
   konfiguracje rope per typ warstwy.
6. **Softcapping logitów** (`softcap * tanh(x / softcap)`) przed samplingiem.
7. **Tokenizer `gemma4`** — nowy typ modelu tokenizera w GGUF.
