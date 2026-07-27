# DeepSeek V4 Flash — stan obsługi w FORGE

Model odniesienia: `nvidia/DeepSeek-V4-Flash-NVFP4` (157 GB, 46 shardów
safetensors, 135 235 tensorów, 43 warstwy pnia + 1 głowa MTP).

Checkpoint zawiera **kompletną implementację referencyjną** w `inference/`
(`model.py`, 827 linii, plus `kernel.py`). Każdy element poniżej jest opisany z
tego kodu, nie z papieru — to on jest źródłem prawdy przy odtwarzaniu matematyki.

## Co jest zrobione

Warstwa opisu modelu: rozpoznanie architektury (`deepseek_v4`), mapa 30 ról na
nazwy tensorów, parametry MoE z `config.json`, przycinanie ról opcjonalnych.

Sprawdzone wykonawczo na prawdziwym checkpoincie
(`crates/forge-formats/tests/deepseek_v4_roles.rs`):

- każdy tensor pnia ma przypisaną rolę (poza sufiksami skal i blokiem MTP),
- każda nazwa, po którą sięgnie loader, istnieje w checkpoincie,
- rozkład kompresorów i indekserów po warstwach zgadza się z `compress_ratios`.

Przy okazji wyszła wada ścieżki HF, naprawiona: opis budowany z `config.json`
deklarował role opcjonalne dla WSZYSTKICH warstw, bo — w odróżnieniu od ścieżki
GGUF — nie widział tabeli tensorów. Dla architektury, w której 2 z 43 warstw nie
mają kompresora, a 22 nie mają indeksera, oznaczało to obietnicę 183 tensorów,
których nie ma. Doszło `ModelDescriptor::prune_absent_optional`.

Warstwa kwantyzacji, zmierzona na prawdziwych tensorach
(`crates/forge-formats/tests/deepseek_v4_experts.rs`):

- **Eksperci NVFP4 → jednobuforowy układ GGUF, bitowo dokładnie.** To zdejmuje
  blokadę rezydencji: ekspert jest teraz samodzielnym blokiem bajtów, więc może
  leżeć w VRAM, w RAM-ie albo na dysku niezależnie od sąsiadów. Sprawdzone dla
  wszystkich 126 kodów skali razy 16 kodów wartości oraz na czterech prawdziwych
  ekspertach o różnych kształtach.
- **Wagi nieekspertowe FP8: kafelkowa skala E8M0 → skala na wiersz, błąd wyjścia
  projekcji 4-12e-7.** FORGE ma kernel FP8 tylko ze skalą na wiersz. Skala E8M0
  jest czystą potęgą dwójki, więc różnicę wykładników można wtopić w same bajty
  E4M3 — zmierzony rozrzut skal w obrębie wiersza to najwyżej JEDNA potęga
  dwójki, więc przesunięcie dotyka jednego bitu wykładnika, a jedyną stratą są
  najmniejsze wartości schodzące do zakresu subnormalnego. Zostaje jeden bajt na
  wagę i istniejący kernel.

  **Odrzucone po pomiarze: przekwantyzowanie na Q8_0.** Kosztowało 5,4e-3 na
  wyjściu projekcji — 11 000 razy więcej, przy tym samym bajcie na wagę. Warto
  zapamiętać dlaczego: założenie, że iloczyn skalarny uśredni błędy po tysiącach
  wag, jest FAŁSZYWE. Błąd Q8_0 jest systematyczny (int8 z jedną skalą na 32
  elementy zeruje wartości dużo mniejsze od maksimum grupy), więc błąd wyjścia
  wychodzi równy błędowi wag, nie mniejszy. Przy 43 warstwach to by się
  kumulowało.

  **Odrzucone: materializacja do f16.** Bez straty dokładności, ale urosłaby te
  wagi z 8,2 do 13,7 GiB przy karcie mającej 16 GiB i 148 GiB ekspertów obok.

Dwie konwencje ustalone POMIAREM, bo obie mylą się bezgłośnie:

- `weight_scale_2` DeepSeeka **mnoży** wynik, podczas gdy
  `weight_global_scale` compressed-tensors przez wynik **dzieli**. Podstawienie
  jednego pod drugie daje wagi rzędu 10^6 zamiast 10^-2 — bez błędu, bez
  ostrzeżenia, po prostu śmieci na wyjściu.
- Kod skali `0x7F` (NaN w E4M3) układ GGUF mapuje na zero, co wyzerowałoby całą
  szesnastkę wag. Jest odrzucany zamiast przepuszczany.

## Oracle numeryczny

`tools/deepseek_v4_oracle.py` liczy referencyjne aktywacje na PRAWDZIWYCH wagach
i zrzuca je do pliku; testy Rusta odtwarzają tę samą matematykę i porównują.
`RMSNorm`, `precompute_freqs_cis` i `apply_rotary_emb` są w nim skopiowane
DOSŁOWNIE z `inference/model.py` — to ma być oracle, a nie druga interpretacja
tego samego kodu.

```
python tools/deepseek_v4_oracle.py /tmp/ds_ref.bin
FORGE_DEEPSEEK_V4_ORACLE=/tmp/ds_ref.bin \
  cargo test -p forge-formats --test deepseek_v4_attention
```

Przypięte i zgodne z referencją (`crates/forge-formats/tests/deepseek_v4_attention.rs`):

| Fragment warstwy | Względne L2 |
|---|---|
| zejście LoRA Q (`wq_a` + `q_norm`) | 1,3e-6 |
| pełna ścieżka Q (per-głowicowa norma + rope) | 1,4e-6 |
| ścieżka KV | 1,3e-6 |
| wyjście uwagi (rope odwrotne + grupowana LoRA) | 2,1e-6 |
| kompresor KV, prefill (okna z zakładką + QAT FP8) | 3,7e-7 |
| kompresor KV, dekodowanie (stan okna między tokenami) | 4,7e-7 |
| kompresor indeksera (Hadamard + QAT FP4) | dokładnie |
| punktowanie indeksera | 1,8e-4 |
| rzadka uwaga po zebranych indeksach (z kotwicą) | 3,0e-7 |
| konstrukcja indeksów prefillu | dokładnie |
| SwiGLU eksperta NVFP4 | 1,8e-6 |
| bramka MoE (wybór i wagi) | dokładnie |
| routing haszowany przez `tid2eid` | dokładnie |
| hyper-connections: redukcja HC | 2,2e-7 |
| hyper-connections: rozprowadzenie po Sinkhornie | 9,6e-8 |
| głowa wyjściowa: redukcja HC | 1,4e-7 |
| głowa wyjściowa: logity | 1,3e-6 |

To jest zarazem walidacja obu konwersji kwantyzacji od końca do końca: projekcje
liczone są na wagach przepuszczonych przez produkcyjne przepakowanie NVFP4 i
przeniesienie FP8 na skalę wierszową.

Wychwycone przy okazji szczegóły, które przy pomyłce nie krzyczą, tylko dają
liczby wyglądające sensownie:

- rope obraca pary SĄSIADUJĄCE `(2i, 2i+1)`, a nie połówki wektora — reszta
  FORGE używa układu NeoX. Osobny test kontrolny potwierdza, że próg
  porównania faktycznie odróżnia oba układy.
- rope obejmuje tylko ostatnie 64 wymiary głowicy z 512, a na wyjściu uwagi jest
  nakładane ODWROTNIE (sprzężenie tego samego obrotu).
- Q dostaje drugą normalizację RMS — per głowica i BEZ wagi — już po projekcji.
- Warstwy z kompresją KV używają YaRN i bazy 160000, pozostałe czystego rope 10000.
- Wyjście uwagi dzieli się na 8 grup, każda z własnym blokiem `wo_a`;
  potraktowanie `wo_a` jako jednej macierzy daje poprawny kształt i złe liczby.
- Bias bramki wpływa WYŁĄCZNIE na wybór top-k; wagi bierze się z wyników bez
  niego, normalizuje do sumy 1 i dopiero mnoży przez `route_scale` 1,5.
- SwiGLU obcina bramkę tylko od góry, a wejście obustronnie — obie operacje
  przed mnożeniem.
- Kompresor o stopniu 4 pracuje na oknach Z ZAKŁADKĄ: projekcja daje dwa razy
  szerszy wektor, którego pierwsza połowa opisuje okno przesunięte o blok wstecz.
- Wyjścia obu kompresorów przechodzą symulację kwantyzacji, którą model przeszedł
  w treningu (QAT): zwykły do FP8 blokami po 64, indeksera do FP4 blokami po 32,
  oba ze skalą zaokrągloną do potęgi dwójki.
- Wynik rotacji Hadamarda wraca do bf16. Pominięcie tego zaokrąglenia przesuwa
  maksimum grupy przez granicę potęgi dwójki i zmienia skalę całej grupy przy
  kwantyzacji FP4 — kosztowało 2,9e-2 na punktowaniu indeksera, zanim zostało
  znalezione.
- Indeks `-1` w liście pozycji oznacza maskę, a nie pozycję; kotwica uwagi wchodzi
  WYŁĄCZNIE do mianownika softmaxu, jako logit o zerowym wektorze wartości.
- Sinkhorn po softmaksie po wierszach robi NAJPIERW normalizację po kolumnach, a
  dopiero potem `iters - 1` pełnych par wiersz+kolumna.
- Redukcja HC w GŁOWIE jest prostsza niż w bloku: sama sigmoida, bez Sinkhorna i
  bez macierzy mieszającej, a skala jest jedna dla wszystkich kopii (w bloku są
  trzy osobne).
- Przy dekodowaniu kompresora rope bierze pozycję POCZĄTKU okna, nie tokenu,
  który je domknął.
- W warstwach haszowanych z tablicy pochodzi WYŁĄCZNIE wybór ekspertów; wagi
  nadal liczy się z wyniku bramki. Pominięcie tego rozróżnienia daje poprawne
  wagi przy błędnych ekspertach.

## Warstwa wykonawcza

Kernele (`kernels/mojo/src/deepseek.mojo`), każdy z testem złotym na GPU
przeciwko referencji CPU: `rmsnorm_head_f16`, `rope_interleaved_f16` (z
wariantem odwrotnym), `hadamard_bf16_f16`, `act_quant_fp8_f16`,
`act_quant_fp4_f16`, `compressor_pool_f16`, `compressor_add_ape_f32`,
`sparse_attn_f16`, `index_score_f16`, `hc_sinkhorn_f32`, `hc_reduce_f16`,
`hc_expand_f16`, `moe_gate_sqrtsoftplus_f16`, plus `gemv_fp8_row_f16_v2` dla
wag ze skalą wierszową.

**Ścieżka uwagi liczy się na GPU** od wejścia warstwy po jej wyjście, zgodna z
referencją na 2,3e-2 (`crates/forge-engine/tests/deepseek_v4_attention_gpu.rs`).
Wagi warstwy wczytują się na urządzenie z prawdziwego checkpointu, przez
produkcyjne konwersje kwantyzacji.

Test złożenia zarobił na siebie od razu — złapał trzy błędy niewidoczne dla
testów pojedynczych kerneli:

- krok tablicy slotów kompresora liczony przez `head_dim` UWAGI, podczas gdy
  indekser ma własny, mniejszy — krok wychodził zerowy,
- kompresor liczony w f16: wyniki bramki wychodzą poza zakres, dają `inf`, a
  softmax z `inf - inf` daje NaN. Referencja mówi o tym jednym zdaniem
  („compression need fp32") i nie jest to kwestia dokładności, tylko poprawności,
- kodowanie pozycji `ape` czytane jako f16, gdy w checkpoincie jest f32.

Diagnoza szła po objawie: pierwsze TRZY tokeny zgadzały się z referencją, a NaN
zaczynał się od czwartego — czyli dokładnie tam, gdzie do uwagi wchodzi pierwszy
wpis skompresowany.

Budowanie kerneli ma tryb przyrostowy (`FORGE_KERNEL_BUILD_ONLY=nazwa,...`):
pełny przebieg to ~55 minut na 518 kerneli, dobudowa pojedynczego — 2 sekundy.
Publikacja pozostaje atomowa. Tryb NIE usuwa artefaktów spoza katalogu:
część kerneli budują osobne skrypty `build_*.mojo`, których parser katalogu nie
zna, więc automatyczne czyszczenie kasowało działające pliki.

## Architektura — co trzeba odtworzyć

Siedem niezależnych mechanizmów, z których FORGE nie ma żadnego.

### 1. Uwaga latentna z projekcjami LoRA

Q idzie przez `wq_a` (4096 → 1024), normę RMS i `wq_b` (1024 → 64 głowice × 512),
po czym jest normalizowane RMS **per głowica** i dostaje rope na ostatnich 64
wymiarach. KV to **jedna** głowica (`wkv`: 4096 → 512) — MQA, nie GQA.

Wyjście uwagi jest dzielone na 8 grup, każda przechodzi przez własny blok `wo_a`
(einsum po grupach), a złączony wynik przez `wo_b`. Do tego rope **odwrotne**
nakładane na wyjście uwagi przed projekcją — rzecz, której nie ma żaden inny
obsługiwany model.

Każda głowica ma skalarną kotwicę (`attn_sink`) doliczaną do softmaxu.

### 2. Dwa strumienie KV

Uwaga czyta jednocześnie okno przesuwne (128 tokenów) **i** strumień
skompresowany. Ten drugi powstaje przez uczony pooling bramkowany: `wkv` i
`wgate` liczą wartości i wagi, `ape` koduje pozycję wewnątrz okna kompresji, a
softmax po oknie daje jeden wpis na `compress_ratio` tokenów.

Stopień kompresji jest per warstwa (`compress_ratios`): naprzemiennie 4 i 128,
z zerem na warstwach 0, 1 i ostatniej. Ratio 4 używa okien **z zakładką** (dwa
razy szerszy stan, przesuwany), ratio 128 nie. Kompresja liczy się w fp32 i ma
własny stan dekodowania (bufory `kv_state` / `score_state`).

### 3. Indekser rzadkiej uwagi

Warstwy o ratio 4 mają drugi kompresor — z rotacją Hadamarda i symulacją FP4 —
którego wyjście służy do punktowania pozycji: `relu(q·k)` ważone per głowica,
sumowane, potem top-512. Wybrane indeksy trafiają razem z indeksami okna do
rzadkiej uwagi.

### 4. Rzadka uwaga po zebranych indeksach

`sparse_attn(q, kv, sink, topk_idxs, scale)` — uwaga liczona wyłącznie po liście
indeksów, z kotwicą. FORGE ma tylko uwagę gęstą i okienną; to jest nowy kernel.

### 5. MoE

256 ekspertów, top-6, jeden ekspert dzielony, `moe_intermediate_size` 2048.
Bramka ma bias (`noaux_tc`) i funkcję `sqrtsoftplus`. Trzy warstwy mają zamiast
tego tablicę `tid2eid` — routing haszowany po identyfikatorze tokena.

Eksperci są zapisani **pojedynczo** (`ffn.experts.{e}.w1/w2/w3`), nie jako jeden
sklejony tensor. To akurat pasuje do rezydencji per ekspert bez żadnej pracy
dodatkowej — patrz `MOE_RESIDENCY.md`.

### 6. Modulacja warunkowana haszem

`hc_attn_*`, `hc_ffn_*`, `hc_head_*` plus `hc_split_sinkhorn` z 20 iteracjami,
`hc_mult` 4, 3 warstwy hasza. Wpinane przy uwadze, FFN i głowie logitów.

### 7. Głowa MTP

Osobny blok z `e_proj`, `h_proj`, `enorm`, `hnorm` i własnym kompletem ekspertów.

## Precyzja

Mieszana, opisana w `quantization_config`:

| Co | Format |
|---|---|
| eksperci routowani | NVFP4, grupa 16, skale `weight_scale` (e8m0) + `weight_scale_2` + `input_scale` |
| pozostałe warstwy liniowe | FP8 e4m3 blokowo (128), skala w siostrzanym `.scale` |
| uwaga, ekspert dzielony, głowa, MTP | bf16 (na liście `ignore`) |

Aktywacje kwantyzowane dynamicznie do FP8 przed każdym GEMM-em.

## Czego brakuje, w kolejności

Matematyka modelu — od wejścia bloku po logity, w prefillu i w dekodowaniu —
jest przypięta testami wobec referencji. Poza zasięgiem oracle'a został tylko
blok MTP.

Zostaje warstwa wykonawcza:

1. **Ścieżka FFN** — eksperty i ekspert dzielony spięte z bramką, oraz routing
   haszowany przez `tid2eid`.
2. **Blok** — hyper-connections spięte wokół uwagi i FFN.
3. **Model** — 43 warstwy, głowa wyjściowa, dekodowanie ze stanem okna.
4. **MTP.**
5. **Wybór top-k indeksera na GPU** — dziś liczony na hoście, co kosztuje jedną
   synchronizację na warstwę.

Każdy z tych punktów da się zwalidować numerycznie przeciwko `inference/model.py`
osobno, i tak trzeba je robić — model, który nie mieści się w VRAM i nie ma
punktu odniesienia, nie da się sprawdzić „na oko" po fakcie.

## Ograniczenie sprzętowe

Model ma 157 GB przy 16 GiB VRAM i 62 GiB RAM, więc nawet po pełnym porcie
większość ekspertów będzie stronicowana z NVMe. Pomiar rezydencji na OLMoE
(`MOE_RESIDENCY.md`) daje przy 39% modelu na dysku spadek 1,4× względem układu
bez dysku — tu udział dysku będzie znacznie wyższy.
