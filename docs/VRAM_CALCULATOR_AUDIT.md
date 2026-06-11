# Audyt kalkulatora VRAM (vLLM / llama.cpp / MLX)

> Audyt zakładki "Advanced" w kreatorze deploy serwisu z modelami: `engine-deploy-wizard.js`
> + `deploy/vram_calculator.rs` + handlery `deploy_vllm_recommend` / `engine_recommend`.
> Liczby zweryfikowane na realnych modelach. Lokalizacje jako `plik:linia` z chwili audytu.

## 1. Streszczenie

Kalkulator VRAM pokazuje "kompletny nonsens" przede wszystkim z powodu jednego błędu fizyki:
traktuje `max_num_seqs` jako mnożnik pamięci KV cache zamiast jako limit współbieżności
schedulera. Dla vLLM `kv_cache_bytes = kv_per_token * max_model_len * max_num_seqs`
(`vram_calculator.rs:377-378`) zawyża KV przy domyślnym `max_num_seqs=256` o czynnik 256x
i raportuje fałszywe OOM — przesuwanie suwaka `max_num_seqs` liniowo skaluje liczbę, która
od niego w ogóle nie powinna zależeć. Drugi objaw użytkownika (`kv_cache_dtype` "pokazuje
nonsens") wynika z tego, że `bytes_per_kv_element` (`vram_calculator.rs:65-71`) rozpoznaje
tylko `fp8=1.0`, a wszystko inne (włącznie z realnymi formatami q8_0/q5_1/q4_0 llama.cpp,
które buildery argumentów rzeczywiście wysyłają do `--cache-type-k/v`) traktuje jako 2.0 —
estymacja i wdrożony serwer rozjeżdżają się nawet 3.56x. Do tego dochodzi: brak opcji
KV-turboquant w UI (q*_0 dla llama.cpp, kv-bits dla MLX), brak modelu MoE (wagi zaniżone ~6x
dla Mixtral), brak klampy replikacji głów KV przy TP>kv_heads, oraz całkowity brak fizyki MLX
(liczona client-side przez syntetyczne fałszywe GPU 4096 GB). Wszystkie trzy silniki — vLLM,
llama.cpp i MLX — są dotknięte, a błędy propagują się zarówno w trybie ręcznym, jak
i automatycznym, ponieważ `auto_fit_config` i live-preview w wizardzie powielają te same wzory.

## 2. Jak to dziś działa

### Ścieżka vLLM / llama.cpp (z fizyką backendu)

```
Wizard (engine-deploy-wizard.js)
  renderStepAdvanced (L806) → auto: renderAutoAlert (L1038); manual: renderAdvancedManualControls (L1102)
  bindAdvancedHandlers → fetchVllmRecommendation (L687-728)
        action 'deployVllmRecommendRequest', body.engine='llama.cpp' tylko dla llama (L699)
  live preview: estimateKvGb (L75-78) = 2*layers*kv_heads*head_dim*ctx*seqs*kvDtypeBytes
              kvDtypeBytes (L81-84): fp8→1, else→2
        ↓ binary CBOR
Handler deploy_vllm_recommend (handlers.rs:3968-4208)
  → auto_fit_config (vram_calculator.rs:860-1089)
        TP/PP: vLLM recommend_parallelism_vram_aware (probe ctx=1024,seqs=1); llama.cpp = (1, gpu_count)
        budżet: kv_budget_per_gpu = usable - weights/parallel - activations (L896)
        kv_seq_factor (L952-957): vLLM=seqs, llama=1
        lock matrix (L961-1064) → applied {ctx, seqs, tp, pp}
  → estimate_vram (L331) → estimate_vllm_vram (L345) | estimate_llamacpp_vram (L488)
        vLLM KV = kv_per_token * ctx * seqs (L377-378); kv_per_gpu = kv/parallel (L393)
        llama KV = kv_per_token * ctx (L500-502); kv_per_gpu = kv/gpus_used (L512)
        fits_per_gpu uses util (L433/L570); fits_total ignoruje util (L435/L571)
  → max_context_for_budget (L1113) / max_concurrent_seqs_for_budget (L1135) — binary search po estimate_vram
        ↓ DeployVllmVramEstimate (message_body.rs:2857)
renderVramCard (L948-1040): 5 kafelków KPI + pasek; pctUsed = total/raw_aggregate (L959)
```

### Ścieżka MLX (wyłącznie client-side, bez fizyki backendu)

`DeployEngine` to enum `Vllm | LlamaCpp` (`vram_calculator.rs:259-263`) — nie ma wariantu `Mlx`.
`estimate_vram` dispatch'uje tylko te dwa (`L331-336`). MLX jest liczony w JS:

```
getAdvancedGpus (engine-deploy-wizard.js:662-684): fabrykuje fałszywe GPU {memory_gb: 4096}
        (komentarz L663-668 przyznaje: realny budżet → BadRequest na ~5 GB workspace vLLM)
fetchVllmRecommendation: NIE ustawia body.engine dla mlx → backend defaultuje DeployEngine::Vllm
        → estimate_vllm_vram liczy wagi po vLLM math, reszta wyniku (fits/max_len) wyrzucana
cachedModelSpec ← rec.model_spec; weightsGb ← rec.vram_estimate.model_weights_gb
computeMlxMaxContext (L733-759): availGb = budgetMb/1024 - weightsGb;
        kvPerTokGb = estimateKvGb({..., max_model_len:1, max_num_seqs:1, kv_dtype_bytes:2})
        tokens = floor(availGb/kvPerTokGb), clamp do max_position_embeddings, floor do 512
renderMlxAdvanced (L778-804): TYLKO pole "memory budget (MB)" + readout
```

`engine_recommend`'s gałąź `"mlx"` (`handlers.rs:4448-4460`) zwraca wyłącznie
`default_max_tokens/temperature/top_p` — zero fizyki pamięci.

## 3. Błędy krytyczne (reproduced=true, critical/major)

### KRYT-1 — VLLM-KV-SEQS-MULTIPLIER [critical] — objaw użytkownika nr 1
**Lokalizacja:** `vram_calculator.rs:377-378`; replikowane w `auto_fit` `kv_seq_factor`
(`L952-957`) i client-side `estimateKvGb` (`engine-deploy-wizard.js:75-78`).

**Co liczy dziś:** `kv_cache_bytes = kv_per_seq_per_token * max_model_len * max_num_seqs`.
Przy domyślnym `max_num_seqs=256` KV jest 256x zawyżone. Przesuwanie suwaka `max_num_seqs`
liniowo skaluje raportowane VRAM — dokładnie objaw "nonsens przy zmianie max_num_seqs".

**Przykład (Qwen2.5-32B bf16, TP=2, ctx=32768, seqs=256, 2×A100-80GB):**
`kv_per_token = 2*64*8*128*2 = 262144 B = 256 KiB`.
- DZIŚ: `kv_cache_bytes = 256KiB * 32768 * 256 = 2048.0 GiB`; `per_gpu_gb = 30.27 (weights/GPU)
  + 1024 (kv/GPU) + 8.03 (act) = 1062.3 GB` → `fits_per_gpu=false`, twardy OOM. Raportowane
  1062 GB to 14x cała 2-GPU maszyna.
- POPRAWNIE: KV to **pula resztkowa**, nie składnik wymagany. `required_fixed = weights/GPU
  30.27 + act ~8 = ~38 GiB ≤ 72 usable` → mieści się. Pula KV ≈ `72 - 38 = 33.7 GiB/GPU`,
  mieści `33.7 GiB / 131072 B (TP-shardowany 128 KiB/token) ≈ 276k tokenów` → ~8 współbieżnych
  pełnych sekwencji 32k.

**Root cause:** vLLM PagedAttention NIE prealokuje siatki `max_num_seqs × max_model_len`;
po załadowaniu wag profiluje aktywacje, potem tworzy JEDNĄ pulę KV = `util*VRAM - weights -
activations`. `max_num_seqs` to limit admission schedulera, nie rozmiar puli.

**Fix:** Rozdziel model na fixed-footprint vs pula. `required = weights + profiled_activations
+ overhead` (BEZ członu `ctx*seqs`). `KV_pool = util*VRAM_per_gpu - weights_per_gpu -
activations_per_gpu`. `pool_tokens = KV_pool_bytes / (kv_per_token / tp_kv_shards)`.
`fits = (required ≤ util*VRAM) AND (pool_tokens ≥ max_model_len)`. Raportuj `pool_tokens`
i wyprowadzoną współbieżność. `max_num_seqs` → wyłącznie cap schedulera, nigdy mnożnik pamięci.
Zastąp client-side `estimateKvGb` modelem puli karmionym z odpowiedzi recommend.

---

### KRYT-2 — VLLM-AUTOFIT-INHERITS-SEQS-INFLATION [critical]
**Lokalizacja:** `vram_calculator.rs:952-957, 964, 1007-1011, 1044-1046, 1067`; konsumowane
przez handler `L4099-4182`.

**Co liczy dziś:** Strona budżetu (`L896`) jest poprawna (pula resztkowa), ale strona popytu
podwójnie liczy przez `seqs` via `kv_seq_factor`. To nie tylko źle raportuje — aktywnie
**przepisuje konfigurację użytkownika**.

**Przykład (Qwen2.5-32B, TP=2, util=0.9, 80GB; user ctx=32768, seqs=64):**
- DZIŚ: `kv_budget_per_gpu = 33.7 GiB`. Demand = `262144 * 32768 * 64 = 512 GiB >> 33.7` →
  `auto_fit` kapuje seqs do `floor(33.7GiB / (256KiB*32768)) = 4`. Domyślna polityka bez
  requestów wymusza `default_seqs = 1` (`L943`).
- POPRAWNIE: pula mieści ~276k tokenów = ~8 pełnych sekwencji 32k; `seqs=64` jest poprawne
  jako cap (vLLM stronicuje). Auto-fit powinien zachować ctx=32768, zachować seqs jako cap
  i raportować osiągalną współbieżność.

**Root cause:** Ten sam błąd fizyki co KRYT-1, ale na stronie popytu auto-fit.

**Fix:** Usuń `kv_seq_factor` dla vLLM (demand fitu = jedna sekwencja `max_model_len`, factor 1
jak llama). Wyprowadź INFORMACYJNĄ współbieżność `= pool_tokens / max_model_len`. To naprawia
też `max_concurrent_seqs_for_budget` (`L1135-1150`).

---

### KRYT-3 — AUTOFIT-DEFAULT-SINGLE-STREAM [critical]
**Lokalizacja:** `vram_calculator.rs:943, 1029-1062`.

**Co liczy dziś:** `default_seqs = 1` (`L943`); gałąź `(false,false)` zwraca `max_num_seqs=1`.
To wizard serwujący (vLLM/sglang/TRT-LLM), a nie REPL jednoosobowy.

**Przykład:** Qwen2.5-7B/24GB → auto_fit zwraca `max_num_seqs=1, max_model_len≈18432`.
Wdrożenie tego do vLLM daje serwer serializujący cały ruch — 4090, który batchuje setki
żądań, obsługuje jedno. Rekomendacja jest gorsza niż domyślne ustawienia vLLM
(`max_num_seqs=256`). Testy `auto_default_prefers_max_ctx_with_single_seq` (`L2480-2526`)
i `user_case_gemma_30b_nvfp4` (`L2777`) jawnie asercjonują `max_num_seqs == 1` jako zamierzone
wyjście.

**Root cause:** Polityka domyślna napisana dla "single-user dev" (komentarz `L932-936`),
niezgodna z produktem serwującym.

**Fix:** `default_seqs` na wartość serwerową (256, lub `max(1, min(256, pool_tokens/min_slot_tokens))`
po naprawie KRYT-1). Single-stream jako opt-in. Trzeba poprawić testy razem z fizyką
(te testy enshrine'ują błędne zachowanie).

---

### KRYT-4 — VLLM-KV-GQA-REPLICATION [critical]
**Lokalizacja:** `vram_calculator.rs:393` (dzielnik `tp*pp` bez klampy `min(TP,kv_heads)`);
ostrzeżenie `L411-418`.

**Co liczy dziś:** `kv_per_gpu = kv_cache_gb / (tp*pp)` — zakłada że KV zawsze maleje liniowo
z TP. Brak klampy `min(kv_heads, tp)`.

**Przykład (Llama-3.1-70B: layers=80, kv_heads=8, head_dim=128, fp16, ctx=8192, seqs=256):**
`kv_total = 327680 B/tok * 8192 * 256 = 640.0 GiB`.
- DZIŚ TP=16: `kv_per_gpu = 640/16 = 40.0 GiB`. TP=64: `640/64 = 10.0 GiB`. Na 16×A100-80
  mówi że mieści się gdy realnie nie.
- POPRAWNIE: efektywne shardy KV = `min(TP, kv_heads) = 8`. `kv_per_gpu = 640/8 = 80.0 GiB`
  (płaskie powyżej TP=8). Zaniżenie = `TP/kv_heads` (przy TP=64 → 8x).

**Root cause:** vLLM replikuje głowy KV gdy `TP > num_key_value_heads` (każdy rank trzyma ≥1
całą głowę); per-GPU KV przestaje maleć, a cluster-total KV ROŚNIE o `TP/min(TP,kv_heads)`.

**Fix:** `kv_tp_shards = (tensor_parallel as u64).min(kv_heads).max(1); kv_per_gpu =
kv_cache_gb / (kv_tp_shards as f64 * pp)`. Gdy `TP>kv_heads`, przemnóż `kv_cache_gb` przez
`TP/kv_heads` dla cluster-total. Ostrzeżenie zamiast obecnego błędnego `kv_heads%TP` (DRUG-3).

---

### KRYT-5 — VRAM-MOE-WEIGHTS [critical] (dotyka też MLX)
**Lokalizacja:** `vram_calculator.rs:77-93` (`estimated_params`); `1264-1265` i `1740-1741`
(parser zawsze ustawia `num_parameters/num_active_parameters = 0`).

**Co liczy dziś:** `per_layer = 4*h^2 + 3*h*i + 2*h` — dokładnie JEDEN ekspert MLP na warstwę.
Parser nigdy nie czyta `num_experts/n_routed_experts/num_local_experts/moe_intermediate_size`,
więc każde safetensors MoE jest widziane jako model gęsty z jednym ekspertem.

**Przykład (Mixtral-8x7B, h=4096, i=14336, l=32, v=32000, 8 ekspertów):**
- DZIŚ: `estimated_params() = 8,047,034,368` → bf16 wagi `8.05B*2 = 15.0 GiB` (kalkulator:
  mieści się na jednym 24GB GPU).
- POPRAWNIE: ~46.7B param → ~87 GiB (potrzebuje 4×24GB). Błąd −82.8%.

(Qwen3-30B-A3B z `moe_intermediate=768` czytanym jako `intermediate_size`: dense=1.65B vs
realne 30.5B, błąd >−90%.)

Komentarz `L350` jawnie mówi "vllm loads ALL experts so we count full params" — ale wzór tego
NIE robi. Tylko ścieżka GGUF (`weights_bytes_override = dokładny rozmiar pliku`) ucieka od tego.
**Dla MLX bug jest krytyczny tak samo**: `computeMlxMaxContext` czyta
`rec.vram_estimate.model_weights_gb` (15 GiB zamiast 87) → `availGb` ogromnie zawyżone →
wizard reklamuje context daleko poza to co się zmieści → gwarantowany OOM przy ładowaniu.

**Fix:** W `parse_hf_config_with_override` czytaj MoE config i ustaw `num_parameters`
z `model.safetensors.index.json` (`metadata.total_size`) albo z MoE-świadomego wzoru:
`per_layer FFN = n_routed_experts * 3 * h * moe_intermediate_size (+ shared)`. Pobierz też
index.json w `fetch_hf_config` (`L1408` — dziś GET-uje tylko `config.json`).

---

### KRYT-6 — VRAM-KV-LLAMA-QUANT-IGNORED [critical] — objaw użytkownika nr 2
**Lokalizacja:** `vram_calculator.rs:65-71` (przez `kv_bytes_per_seq_per_token` `L1107`);
builder argów `L1391-1401`.

**Co liczy dziś:** `bytes_per_kv_element` zwraca 2.0 dla każdej etykiety ≠ fp8 (arm `_ => 2.0`).
Wybranie q4_0/q5_1/q8_0 zostawia estymację KV identyczną jak f16, ale
`build_llamacpp_args_string` przekazuje etykietę dosłownie do `--cache-type-k/--cache-type-v`,
więc serwer alokuje znacznie mniejszą pulę niż pokazuje kalkulator.

**Przykład (Qwen2.5-32B, n_ctx=8192, np=1):**
`kv_per_token f16 = 256 KiB/tok → KV = 2.000 GiB`. Wybór q4_0:
- DZIŚ: estymacja nadal `2.000 GiB` (arm `_ => 2.0`).
- POPRAWNIE: `2*64*8*128*0.5625 = 73728 B/tok → 0.5625 GiB`. Przeszacowanie 3.56x — błędnie
  odrzuca konfigurację, która się mieści.

**Tabela (block_bytes/32, z ggml-common.h, zweryfikowane static_asserts):** f16/bf16=2.0;
q8_0=34/32=1.0625; q5_1=24/32=0.75; q5_0=22/32=0.6875; q4_1=20/32=0.625; q4_0=18/32=0.5625;
iq4_nl=18/32=0.5625. Błąd jest obecny w trybie ręcznym I automatycznym
(auto_fit/`max_context_for_budget` przechodzą przez tę samą funkcję).

**Fix:** Pojedyncze źródło prawdy `kv_bytes_per_element(engine, k_type, v_type) -> (f64, f64)`
z osobnymi typami K i V (`KV_per_token = layers*kv_heads*head_dim*(bytes_k + bytes_v)`),
używane przez estymację, auto_fit I builder argów. Quantyzowane V wymaga `-fa` — dodawaj `-fa`
automatycznie. (Patrz §5/§6.)

---

### KRYT-7 — LCPP-NP-CTX-SPLIT [critical] — drugie "nonsens przy zmianie max_num_seqs" dla llama.cpp
**Lokalizacja:** `vram_calculator.rs:500-502, 1364-1404` (build args), `952-957`.

**Co liczy dziś:** Estymacja ustawia `n_ctx = max_model_len`, KV = `kv_per_token * n_ctx`
(bez seqs). Builder emituje `-c max_model_len` i `-np max_num_seqs` dosłownie. W llama-server
`-c` to CAŁKOWITY kontekst dzielony na `-np` slotów: `n_ctx_seq = n_ctx / n_seq_max`
(zweryfikowane: `llama-context.cpp:209`; presety upstreamu `arg.cpp:3983 n_ctx = 2048*n_parallel`).

**Przykład (Qwen2.5-32B, użytkownik chce 8 współbieżnych × 8192 ctx):**
- DZIŚ: emituje `-c 8192 -np 8` → każdy slot dostaje `8192/8 = 1024` tokenów (7/8 okna znika).
  KV raportowane = `0.25 MiB * 8192 = 2.0 GiB` i NIE zmienia się gdy seqs 1→8→256.
- POPRAWNIE: `-c 65536 -np 8` → każdy slot 8192 tokenów; KV = `0.25 MiB * 65536 = 16.0 GiB`.
  8x różnica jest niewidoczna w kalkulatorze.

**Root cause:** Mapowanie `(max_model_len, max_num_seqs)` → argi llama-server myli "całkowity
kontekst" z "kontekstem per-request".

**Fix:** Zdefiniuj `max_model_len` jako kontekst per-request. W estymacji ustaw
`n_ctx = max_model_len * max_num_seqs` (kv_seq_factor=seqs dla llama). W builderze emituj
`-c (max_model_len*max_num_seqs) -np max_num_seqs`. Pogodź client `estimateKvGb` (już mnoży
przez seqs) z backendem (który teraz też powinien).

---

### KRYT-8 — LCPP-ROW-SPLIT-KV-EVEN [critical w trybie auto, major w manualu]
**Lokalizacja:** `vram_calculator.rs:507-522, 534-541` (estymacja); `900-902` (auto_fit budget).

**Co liczy dziś:** Dla `tp>1` (→ `--split-mode row`) estymacja dzieli KV równo (`kv/gpus_used`)
i tylko wypycha ostrzeżenie (`L534-541`), ale LICZBA się nie zmienia. Auto_fit mnoży budżet KV
przez `parallel` dla WSZYSTKICH trybów split (`L900-902`) — poprawne tylko dla layer-split.

**Przykład (Qwen2.5-32B-Q4 ~20 GiB wag, ctx=32768 f16 KV=8.0 GiB, 4 GPU, tp=4 row, karty 8GB):**
- DZIŚ per_gpu = `20/4 + 8/4 + 0.36 + 0.40 = 7.76 GiB` → "mieści się na 8GB". W trybie auto
  budżet = `4× per-card` → wybiera kontekst, którego pełne KV nie zmieści się na main GPU →
  gwarantowany OOM.
- POPRAWNIE: pod row-split KV + attention zostają na main GPU. `main_gpu = 20/4 + 8.0
  (PEŁNE KV) + 0.36 + 0.40 = 13.76 GiB` → OOM na karcie 8GB. `fits_per_gpu` musi testować
  szczyt main GPU.

**Fix:** W gałęzi `tp>1` (row): `main_gpu = weights/gpus + FULL kv + compute + cuda`;
`fits_per_gpu = max(main, secondary)`. W auto_fit: dla row-split budżet KV =
`kv_budget_per_gpu` (jedna karta), nie `*parallel`. Layer-split (pp>1) zachowuje równy podział.

## 4. Błędy drugorzędne i not-a-bug

| ID | Sev | Lokalizacja | Streszczenie |
|----|-----|-------------|--------------|
| DRUG-1 VLLM-FITS-TOTAL-IGNORES-UTIL | major | `L435`, `L571` | `fits_total = total ≤ each*count` ignoruje `util` (które `fits_per_gpu` stosuje, `L433`). 2×80GB util=0.9: budżet 160 zamiast 144. Config 150 GB raportuje fits=true, realnie OOM. Również: `fits_total` sumuje VRAM jak jedną pulę (ignoruje rozmieszczenie PP/per-GPU). Pasek `pctUsed` w UI (`engine-deploy-wizard.js:959`) używa tej samej fungible-pool. **Fix:** pomnóż RHS przez `util` i bramkuj na `fits_per_gpu`; rozważ usunięcie `fits_total`. |
| DRUG-2 VLLM-WORKSPACE-5GB-FLAT | major | `L395, L892, L798` | Non-KV overhead = `5.0 + 10%*weights/GPU` — wymiarowo źle (aktywacje skalują się z `max_num_batched_tokens*hidden*dtype`, nie z param count). Dla 0.5B: 5.12 GB > sam model; dla 70B TP=8: 6.63 GB zamiast ~3-4. **Fix:** `non_kv = cuda_graph_const(~1-2GB) + nccl(~0.3*tp) + activation_peak(max_num_batched_tokens, hidden, dtype)`; dodaj `max_num_batched_tokens` do `VramEstimateInput`. |
| DRUG-3 VLLM-RECOMMEND-KVHEADS-OVERRESTRICT | major | `L740, L782, L635/L647-649`, ostrzeżenie `L411-418` | Pickery wymagają `kv_heads%TP==0` jako twardy dyskwalifikator i ostrzeżenie "vLLM odrzuci konfiguracje" — FAŁSZ. vLLM wymaga tylko `num_attention_heads%TP==0`; `TP>kv_heads` jest legalne (replikacja). Steeruje z TP=16 do TP=8/PP=2. **Fix:** usuń `kv_heads%TP` z filtra; zmień ostrzeżenie na informacyjne. |
| DRUG-4 VLLM-PROBE-UNDERCOUNTS-KV | major | `L794-795` | Probe TP/PP z `ctx=1024, seqs=1, kv_dtype='auto'` — wybiera najwęższy TP mieszczący wagi, ignoruje realny ctx/seqs i wybrany `kv_cache_dtype`. **Fix:** probe z docelowym ctx/seqs i realnym kv_dtype, albo re-waliduj wybrany TP po auto_fit. |
| DRUG-5 AUTOFIT-TWO-CALLERS-DIVERGE | major | `handlers.rs:4080-4097` vs `4290-4304` | `engine_recommend` hardkoduje `kv_cache_dtype='auto'`, `util=0.9`, wszystkie locki=false, `weights_override=None` — ignoruje wybory użytkownika. Te same dane → różne odpowiedzi zależnie od transportu. **Fix:** jeden wspólny builder `AutoFitRequest`. |
| DRUG-6 AUTOFIT-SEQSLOCK-CTX-FLOOR | major | `L1018-1027` | Korolarium KRYT-1: lock seqs=64 → `max_ctx = budget/(kv_per_token*64) = 291 → clamp 512`. Rekomenduje 512-tokenowy kontekst dla 7B serwującego 64 użytkowników. **Fix:** naprawienie `kv_seq_factor` rozwiązuje to. |
| DRUG-7 AUTOFIT-LLAMA-COMPUTEBUFFER-ZERO | major | `L893, L477-482` | Gdy GGUF spec ma `vocab/hidden=0`, compute buffer=0, budżet KV over-credited; `estimate_llamacpp_vram` ostrzega (`L563`), auto_fit nie. **Fix:** guard `vocab==0||hidden==0` w auto_fit albo floor ~0.5 GB. |
| DRUG-8 VRAM-PARAM-FORMULA-DENSE | major | `L90-92` | `4*h^2` ignoruje GQA (+75% na członie attention); `lm_head=v*h` dodawane bezwarunkowo podwójnie liczy embed gdy `tie_word_embeddings=true`. Qwen2.5-7B: 8.232B vs 7.616B (+8.1%); Qwen2.5-0.5B (tied): 663M vs 494M (+34.2%). **Fix:** GQA-poprawny attn, czytaj `tie_word_embeddings`. |
| DRUG-9 VRAM-AWQ-GPTQ-BYTES | major | `L124` | 4-bit=0.5625 (4.5 bit) płasko na cały model; AWQ/GPTQ g128 to ~4.156 bit (0.5195) ale trzymają embed/lm_head/norms w fp16. Net dla 7B-AWQ: ~−17% (real ~5.19 GiB vs calc 4.31 GiB). **Fix:** dwa kubełki (quant linears vs fp16 reszta) albo ufaj index total_size. |
| DRUG-10 WIZ-MANUAL-ARGS-VLLM-ONLY | critical→efekt zależny | `engine-deploy-wizard.js:2266-2283` | Tryb manual zawsze emituje flagi vLLM, nawet dla llama.cpp (brak gałęzi `isLcpp`). Docker deploy (`docker.rs:703-714` czyta `vllm_args` bezwarunkowo) → llama-server dostaje `--max-model-len`/`--enable-chunked-prefill`. Native python-bundle DROPUJE je (gubi ctx/seqs/KV). **Fix:** gałąź `isLlamaCppEngine()` budująca `-c/-ngl/-b/-np/--split-mode/--cache-type-k/v`. |
| DRUG-11 VRAM-KV-VLLM-FP16-VERBATIM | major | `L1314-1317` | `--kv-cache-dtype fp16`/`bfloat16` to nieprawidłowe tokeny vLLM (tylko `auto/fp8/fp8_e4m3/fp8_e5m2`) → serwer nie wstaje. **Fix:** ogranicz opcje vLLM, emituj tylko dla rodziny fp8. |
| DRUG-12 VRAM-KV-LLAMA-FP16LABEL-INVALID | major | `L1392-1401` | `fp16`/`bfloat16` przekazane dosłownie do `--cache-type-k/v` — llama wymaga `f16`/`bf16`. **Fix:** normalizuj `fp16→f16`, `bfloat16→bf16`. |
| DRUG-13 WIZ-MEMUTIL-LLAMACPP | minor | `engine-deploy-wizard.js:1242` | Suwak `gpu_memory_utilization` pokazany dla llama.cpp. Util **DZIAŁA** na fits llama (`L569-570`), więc zmienia werdykt, ale NIE dociera do serwera (brak flagi). Wprowadza w błąd. **Fix:** dla llama eksponuj `-ngl` zamiast util. |
| LCPP-UBATCH-B-FLAG | major | `L1371-1372, 477-482` | `-b 512` to LOGICAL batch (źle obniża przepustowość prefilla); fizyczny `-ub` (default 512, który driveuje compute buffer) nigdy nie emitowany. Logits liczone `vocab*ubatch*4` zawyżają decode ~512x (server liczy logits tylko dla ostatniego tokena per seq). **Fix:** emituj `-ub 512` (lub nic), modeluj logits jako `vocab*n_active_seq*4`. |

**Błędy minor (poprawna fizyka, mała magnituda):** VRAM-MXFP4-BYTES (`L122-124`: mxfp4 powinno
0.5312 nie 0.5625, +5.9%); VRAM-FP8-DTYPE-DEFAULT (`L54-62`: `float8_e4m3fn`/`uint8` → 2.0
zamiast 1.0, 2x zawyżenie raw fp8); VRAM-FP8-OVERHEAD (`L126-127`: 8-bit=1.0625 zawyża fp8,
realnie ~1.0002 dla block-128 — DeepSeek-V3 671B: +42 GiB); VRAM-KV-FP8-OVERHEAD (`L67`:
fp8=1.0 ale llama mapuje na q8_0=1.0625, −6.25%); VLLM-GIB-GB-UNIT-MISMATCH (`L1865-1867`:
GiB vs decimal-GB, ~7.4% — latentne jeśli katalog odzwierciedla nvidia-smi); VLLM-PP-NO-EMBED-BUBBLE
(`L389/393`: PP nie dzieli embed/lm_head, boundary stages ~2-4 GiB cięższe); WIZ-KV-KPI-SUB-DESYNC
(`L1017`: caption KV nie aktualizuje się z suwakiem, kosmetyka); AUTOFIT-ERROR-MSG-NONACTIONABLE
(`L982-990`: komunikat dziedziczy zawyżoną liczbę).

**Not-a-bug / sprostowania:**
- "turboquant" **nie istnieje** jako nazwana funkcja w repo (grep zwraca tylko Whisper
  `large-v3-turbo` i jeden `NVFP4-turbo` w nazwie repo testowego). To czego użytkownik chce to
  **realny quantyzowany KV cache**: llama.cpp `--cache-type-k/v q*_0`, MLX `--kv-bits`,
  vLLM `fp8` — wszystkie REALNE, ale o różnych formatach. "Działa na obu" jest prawdą tylko
  w sensie luźnym.
- WIZ-KVDTYPE-PREVIEW-DESYNC: dla wartości które UI faktycznie produkuje (auto/fp16/bf16/fp8)
  preview i backend zgadzają się — realny defekt to brak opcji q*_0 i desync fp8→q8_0
  (oba minor). Skorygowano z major na minor.
- Aktywacje vLLM cluster-total (`L396` = `act_per_gpu*parallel`) są spójnym roll-upem per-GPU
  — to nie podwójne liczenie.

## 5. Braki funkcjonalne — rzeczywistość KV-quant per silnik

| Funkcja | vLLM | llama.cpp | MLX |
|---------|------|-----------|-----|
| **KV-turboquant** | tylko `fp8`/`fp8_e4m3`/`fp8_e5m2` (=1.0 B) — brak int4/int8 KV w core | `q8_0=1.0625`, `q5_1=0.75`, `q5_0=0.6875`, `q4_1=0.625`, `q4_0=0.5625`, `iq4_nl=0.5625` | `--kv-bits 8` (~1.0625), `--kv-bits 4` (~0.5625, z `--kv-group-size`) |
| **Osobne K/V** | nie | TAK (typowo K=q8_0, V=q4_0) — `build_llamacpp_args` pisze oba flagi ale wymusza równe | nie |
| **Flash-attn wymagany** | n/d | TAK dla quantyzowanego V — `-fa` musi być emitowane | n/d |
| **Obecne w UI** | tylko auto/fp16/bfloat16/fp8 (z czego fp16/bfloat16 są NIEPRAWIDŁOWE) | te same 4 (żadna q*_0!) | brak — hardkod fp16 |

**Pozostałe braki:**
- **MLX jako realny silnik:** brak `DeployEngine::Mlx`; cała pamięć liczona client-side przez
  fałszywe GPU 4096 GB. Manual mode (ustaw seqs/kv-dtype i zobacz poprawną liczbę) jest
  niemożliwy. `engine_recommend` "mlx" (`handlers.rs:4448-4460`) zwraca tylko sampling params.
  MLX hardkoduje `max_num_seqs=1` (`engine-deploy-wizard.js:744,756`) — mlx-lm wspiera batched
  generation. MLX kv-bits hardkodowane fp16 mimo że mlx-swift 3.x ma `QuantizedKVCache` —
  komentarz `L732` "mlx-swift nie ma kwantyzacji KV cache" jest nieaktualny.
- **MLX wired-limit:** `availGb = budgetMb/1024 - weightsGb` (`L748`) traktuje surowe MB jako
  całą pulę; brak `iogpu.wired_limit_mb`, brak rezerwy OS, brak scratch. Default
  `mlx_max_memory_mb=8192` mniejszy niż wagi większości modeli 7B+ → wizard otwiera się
  w permanentnym overflow.
- **MLX quant key:** `detect_quantization` (`L247`) czyta tylko `quantization_config`;
  MLX-community trzyma top-level `quantization: {bits, group_size}` — ignorowane. Repo bez
  `-4bit` w nazwie → wykryte jako bf16 (~3.55x zawyżenie). `quant_label_to_bytes` ignoruje
  `group_size` (g32 4bit=0.625 nie 0.5625).
- **MoE param parsing:** patrz KRYT-5.
- **GQA-aware param counting:** patrz DRUG-8.
- **kv_heads<TP replikacja:** patrz KRYT-4.

## 6. Docelowy projekt kalkulatora

### (a) Jedno źródło prawdy dla bajtów

```rust
// Wagi — zależne od engine'u tylko przez nazwę etykiety
fn weight_bytes_per_param(quant_label, dtype, group_size) -> f64
// 4bit: bits/8 + 4.0/group_size (g64→0.5625, g32→0.625); mxfp4→0.5312; nvfp4→0.5625
// 8bit fp8: ~1.0002; int8-group: 1.0195; float8_e4m3fn/uint8 dtype → 1.0

// KV — engine-aware, osobne K i V
fn kv_bytes_per_element(engine, label) -> Option<f64>
// vLLM:  {auto/bf16/f16: 2.0, fp8/fp8_e4m3/fp8_e5m2: 1.0}  (inne → None = invalid)
// llama: {f16/bf16: 2.0, q8_0: 1.0625, q5_1: 0.75, q5_0: 0.6875,
//         q4_1: 0.625, q4_0: 0.5625, iq4_nl: 0.5625}
// MLX:   {none: 2.0, kv8: 1.0625, kv4: 0.5625}
fn kv_per_token(model, engine, k_type, v_type) =
    layers * kv_heads * head_dim * (kv_bytes(engine,k) + kv_bytes(engine,v))
```

Ta sama tabela karmi estymację, auto_fit I builder argów — etykieta → token CLI mapowana
z tego samego miejsca, więc estymacja i wdrożona komenda NIGDY się nie rozjadą. Builder
normalizuje (`fp16→f16` dla llama, `fp8→nic` dla auto vLLM) i dodaje `-fa` gdy V jest quantyzowane.

### (b) Poprawna fizyka per silnik

**vLLM — model puli:**
```
required_fixed/GPU = weights/parallel + activation_peak(max_num_batched_tokens, hidden, dtype)
                     + cuda_graph_const + nccl(0.3*tp)
kv_tp_shards = min(tensor_parallel, kv_heads).max(1)
kv_per_token_per_gpu = kv_per_token / kv_tp_shards          // PP dzieli warstwy osobno
KV_pool/GPU = util*VRAM/GPU - required_fixed/GPU
pool_tokens = KV_pool_bytes / kv_per_token_per_gpu
fits = (required_fixed ≤ util*VRAM) AND (pool_tokens ≥ max_model_len)
concurrent_full_len = pool_tokens / max_model_len           // INFORMACYJNE
```
Raportuj: `weights`, `activations`, `kv_pool_gb`, `pool_tokens`, `concurrent_full_len_seqs`.
`max_num_seqs` = wyłącznie cap schedulera. Komunikat "max seq len larger than KV cache" gdy
`pool_tokens < max_model_len`.

**llama.cpp — semantyka -c/-np:**
```
n_ctx = max_model_len * max_num_seqs                         // -c
KV = kv_per_token(k,v) * n_ctx
row-split (tp>1): main_gpu = weights/gpus + FULL_kv + compute + cuda; secondary = weights/gpus + cuda
layer-split (pp>1): kv/gpus, weights/gpus równo
fits_per_gpu = max_over_gpus(per_gpu) ≤ util*VRAM
compute_buffer: logits = vocab * n_active_seq * 4 (nie *ubatch); scratch = ub*hidden*4*6
```
Builder: `-c (max_model_len*max_num_seqs) -np max_num_seqs -ngl 999 -ub 512 --split-mode ...
--cache-type-k <k> --cache-type-v <v> [-fa]`.

**MLX — realny silnik + unified wired-limit:**
```rust
DeployEngine::Mlx => estimate_mlx_vram(model, input) {
    weights = quant_weights(group_size-aware)
    kv = kv_per_token(model, Mlx, kv_bits) * ctx * max_num_seqs
    scratch = graph_const
    budget = min(user_mlx_max_memory_mb, wired_limit) - reserved_os
    fits = weights + kv + scratch ≤ budget
}
```
Bez 5GB workspace, bez TP/PP (single device). Wizard wysyła `engine='mlx'` i realny budżet;
usuń fałszywe GPU 4096 GB.

### (c) Poprawny TP/PP sharding
- Wagi: `/(tp*pp)` (poprawne).
- KV vLLM: `/(min(tp,kv_heads)*pp)`; cluster-total `*= tp/min(tp,kv_heads)` gdy `tp>kv_heads`.
- PP boundary stages: dodaj `embed` (stage 0) i `lm_head` (last) do najcięższego stage;
  `per_gpu = max over stages`.
- Twarda admisja vLLM: `num_attention_heads%tp==0` i `layers%pp==0`. `kv_heads%tp` = nota
  informacyjna (replikacja), NIE dyskwalifikator.

### (d) Manual + auto z jednego silnika
`auto_fit_config` i `estimate_vram` muszą używać DOKŁADNIE tej samej funkcji puli/KV (dziś
probe `/parallel` tiny-ctx, auto_fit budget `/1`, estimate `/parallel`, client `bez TP` —
cztery różne księgowania). Oba handlery (`deploy_vllm_recommend`, `engine_recommend`) budują
`AutoFitRequest` z tych samych pól wizarda. Client-side `estimateKvGb` zastąpiony wartościami
z odpowiedzi recommend (lub współdzieli identyczną tabelę bajtów). Wtedy ręczny i automatyczny
tryb nigdy się nie rozjadą.

### (e) Kontrolki UI per silnik
- **vLLM/sglang:** max-model-len, max-num-seqs (cap), TP/PP, gpu_memory_utilization,
  kv-cache-dtype `{auto, fp8, fp8_e4m3, fp8_e5m2}`, max-num-batched-tokens, chunked-prefill.
  Readout: "pula KV X GiB → N tokenów → ~M współbieżnych pełnych sekwencji".
- **llama.cpp:** -c (per-request ctx), -np, -ngl, --split-mode, **osobne** cache-type-k/cache-type-v
  `{f16, bf16, q8_0, q5_1, q5_0, q4_1, q4_0, iq4_nl}`, flash-attn toggle (auto-on gdy V
  quantyzowane). BEZ gpu_memory_utilization.
- **MLX:** memory budget (z wykrytym wired-limit i sugerowanym z wag), kv-bits `{none, 8, 4}`
  + group-size, max-num-seqs, quant override. Nota: brak TP/PP (single device).
- Wspólne: chipy presetów KV-turboquant ("fp8 2× ctx" vLLM; "q4_0 + flash-attn" llama; "kv4"
  MLX) wzorowane na chipach ctx (`L1090-1100`). Kafelek KV pokazuje EFEKTYWNY wdrożony typ
  ("fp8 → q8_0, 1.06 B/elem").

## 7. Plan wdrożenia (fazowany)

### Faza 0 — Tabela bajtów jako źródło prawdy [RYZYKO: niskie]
**Pliki:** `vram_calculator.rs`.
- Zastąp `bytes_per_kv_element` (`L65-71`) → `kv_bytes_per_element(engine, label) -> Option<f64>`
  z tabelą §6(a). Przerób `kv_bytes_per_seq_per_token` (`L1094-1109`) na `(bytes_k + bytes_v)`.
- Dodaj `mxfp4=>0.5312` (`L122`), `float8_e4m3fn/float8_e5m2/uint8=>1.0` (`L54-62`), rozdziel
  fp8 (1.0002) od int8-group (1.0195) (`L126-127`).
- **Testy:** parametryzowane per (engine, label) → oczekiwane bajty; istniejące testy
  `L2563-2596`, `L2960-2965` ASERCJONUJĄ błędne 0.5625/1.0625 — zaktualizować, nie obchodzić.

### Faza 1 — Fizyka vLLM: model puli [RYZYKO: średnie — rdzeń]
**Pliki:** `vram_calculator.rs`, `message_body.rs`.
- Przepisz `estimate_vllm_vram` (`L345-466`): usuń `*max_num_seqs` z KV (`L377-378`); rozdziel
  `required_fixed` vs `KV_pool`; dodaj klampę `min(tp,kv_heads)` (`L393`); popraw `fits_total *= util` (`L435`).
- Zastąp 5GB-flat aktywacje modelem `f(max_num_batched_tokens, hidden, dtype)` (`L395`); dodaj
  `max_num_batched_tokens` do `VramEstimateInput` (`L280-292`).
- Dodaj pola do `DeployVllmVramEstimate` (`message_body.rs:2857`): `kv_pool_gb, pool_tokens,
  concurrent_full_len_seqs`.
- **Testy:** Qwen2.5-32B TP=2 → `per_gpu ≈ 38 GiB` nie 1062; Llama-70B TP=16 → `kv_per_gpu=80`
  nie 40; fits_total z util.

### Faza 2 — auto_fit jednolity [RYZYKO: średnie]
**Pliki:** `vram_calculator.rs`, `handlers.rs`.
- Usuń `kv_seq_factor=seqs` dla vLLM (`L952-957`); demand fitu = jedna sekwencja; `default_seqs`
  na 256 (`L943`); polityka `(false,false)` (`L1029-1062`) przestaje pinować seqs=1.
- llama.cpp: `kv_seq_factor=seqs`, `n_ctx=ctx*seqs`; budżet row-split = `/1` nie `*parallel`
  (`L900-902`); guard `vocab/hidden==0` (`L893`).
- Probe TP/PP (`L787-805`) z docelowym ctx/seqs i realnym kv_dtype.
- Ujednolić `engine_recommend` (`handlers.rs:4290-4304`) z `deploy_vllm_recommend` (`4080-4097`)
  — wspólny builder `AutoFitRequest`; dodać brakujące pola do `EngineRecommendRequest`.
- **Testy:** lock seqs=64 nie daje ctx=512; auto i estimate zwracają zgodne KV dla TP>1;
  oba handlery zgodne dla identycznych danych. **Zaktualizować** testy `L2480-2526`, `L2777`.

### Faza 3 — Fizyka llama.cpp [RYZYKO: średnie]
**Pliki:** `vram_calculator.rs`.
- `estimate_llamacpp_vram` (`L488-601`): `n_ctx=ctx*seqs` (`L500-502`); row-split main-GPU
  pełne KV, `fits=max(main,secondary)` (`L507-522`); logits=`vocab*n_active_seq*4` (`L477-482`).
- `build_llamacpp_args_string` (`L1364-1404`): `-c (ctx*seqs)`; osobne K/V; `-ub 512` zamiast
  `-b 512`; `-fa` gdy V quant; normalizacja etykiet.
- **Testy:** `-c 8192 -np 8` → `n_ctx_seq=8192` po fixie; q4_0 KV = 0.5625× f16; row-split
  main-GPU fits.

### Faza 4 — MoE / param count [RYZYKO: średnie]
**Pliki:** `vram_calculator.rs`.
- `parse_hf_config_with_override` (`L1162-1267`): czytaj MoE fields, `tie_word_embeddings`;
  pobierz `model.safetensors.index.json` w `fetch_hf_config` (`L1408`) → `num_parameters`
  z `metadata.total_size`.
- MoE-aware `estimated_params` (`L77-93`); GQA-poprawny attn; warunkowy lm_head.
- **Testy:** Mixtral-8x7B → ~46.7B nie 8.05B; Qwen2.5-7B → 7.616B; tied 0.5B → ~494M.

### Faza 5 — MLX realny silnik [RYZYKO: średnie-wysokie]
**Pliki:** `vram_calculator.rs`, `handlers.rs`, `message_body.rs`.
- Dodaj `DeployEngine::Mlx` (`L260`) + `estimate_mlx_vram` (§6); dispatch (`L331`); wired-limit
  + scratch.
- `detect_quantization` (`L232-253`): czytaj top-level `quantization` + group_size.
- Wizard: wyślij `engine='mlx'` + realny budżet; usuń fałszywe GPU (`engine-deploy-wizard.js:669-672`);
  `computeMlxMaxContext` woła backend.
- **Testy:** mlx-community 4bit/g32 → 0.625; name-less `-q4` repo nie wykryte jako bf16;
  kv-bits zmienia max-context.

### Faza 6 — UI per silnik [RYZYKO: niskie-średnie]
**Pliki:** `engine-deploy-wizard.js`.
- Engine-aware KV select (`L1255-1260`) + osobne K/V dla llama; flash-attn toggle; chipy turboquant.
- `isLlamaCppEngine()` w startDeploy (`L2266-2283`); `gpu_memory_utilization` tylko vLLM;
  `-ngl` dla llama.
- Zastąp `estimateKvGb`/`kvDtypeBytes` (`L75-84`) wartościami z recommend; aktualizuj `.k-sub`
  w `updateLiveKvTile` (`L1333-1354`); suwak seqs cap = 256 (nie `max_supported_num_seqs`).
- MLX panel (`L778-804`): kv-bits, max-num-seqs, quant override, wykryty unified RAM.
- **Testy E2E (Playwright):** przesuwanie max_num_seqs nie zmienia paska KV (vLLM); wybór q4_0
  (llama) halvuje KV; manual llama emituje `-c/-np` nie `--max-model-len`; MLX kv4 zmienia context.

**Kolejność krytyczna:** Faza 0 → 1 → 2 są ładujące (~90% "nonsensu" znika po nich). Fazy 3-6
budują na ujednoliconej tabeli i modelu puli.
