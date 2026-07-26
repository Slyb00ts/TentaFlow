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
- **Wagi nieekspertowe FP8 → Q8_0, względne L2 = 5,5e-3.** Materializacja do f16
  — czyli dotychczasowa ścieżka FP8 — urosłaby te wagi z 8,2 do 13,7 GiB przy
  karcie mającej 16 GiB i 148 GiB ekspertów do zmieszczenia obok. Q8_0 zajmuje
  1,06 bajta na wagę i wchodzi we wszystkie istniejące kernele bez pisania
  nowych. Maksymalny błąd względny sięga 1,0 na pojedynczych wartościach
  wielokrotnie mniejszych od maksimum swojej grupy — int8 z jedną skalą je
  zeruje; ich wkład do L2 jest znikomy, ale to jest realne obcięcie, nie
  zaokrąglenie. Natywny kernel FP8 z kafelkową skalą zdejmie ten koszt później.

Dwie konwencje ustalone POMIAREM, bo obie mylą się bezgłośnie:

- `weight_scale_2` DeepSeeka **mnoży** wynik, podczas gdy
  `weight_global_scale` compressed-tensors przez wynik **dzieli**. Podstawienie
  jednego pod drugie daje wagi rzędu 10^6 zamiast 10^-2 — bez błędu, bez
  ostrzeżenia, po prostu śmieci na wyjściu.
- Kod skali `0x7F` (NaN w E4M3) układ GGUF mapuje na zero, co wyzerowałoby całą
  szesnastkę wag. Jest odrzucany zamiast przepuszczany.

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

1. **Mikser: uwaga latentna** z LoRA, rope odwrotnym na wyjściu i kotwicą.
2. **Podwójny strumień KV** — okno plus kompresor, ze stanem dekodowania.
3. **Indekser i rzadka uwaga** po zebranych indeksach.
4. **Bramka MoE** z biasem, `sqrtsoftplus` i routingiem haszowanym.
5. **Hash-conditioning** z Sinkhornem.
6. **MTP.**
7. **Natywny FP8 z kafelkową skalą** — zdejmuje przekwantyzowanie do Q8_0.

Każdy z tych punktów da się zwalidować numerycznie przeciwko `inference/model.py`
osobno, i tak trzeba je robić — model, który nie mieści się w VRAM i nie ma
punktu odniesienia, nie da się sprawdzić „na oko" po fakcie.

## Ograniczenie sprzętowe

Model ma 157 GB przy 16 GiB VRAM i 62 GiB RAM, więc nawet po pełnym porcie
większość ekspertów będzie stronicowana z NVMe. Pomiar rezydencji na OLMoE
(`MOE_RESIDENCY.md`) daje przy 39% modelu na dysku spadek 1,4× względem układu
bez dysku — tu udział dysku będzie znacznie wyższy.
