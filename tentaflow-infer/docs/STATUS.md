# FORGE — Status realizacji vs SPEC

Uczciwa inwentaryzacja tego, co jest zrobione, częściowe i nietknięte, mapowana
na sekcje `docs/SPEC.md`. **Reguła utrzymania: aktualizuj ten plik gdy domykasz
lub zaczynasz element (w tym samym commicie).**

Skala: SPEC to plan na ~30-45 inż. × 14 mies. (7 streamów). Zrobiony jest
najtrudniejszy RDZEŃ jednokartowy (kernele, silnik, KV, batching, kwantyzacja)
— produkcyjnej jakości, bramkowany testami. Poniżej reszta.

Legenda: ✅ zrobione · 🟡 częściowe · ❌ nietknięte

Ostatnia aktualizacja: 2026-07-22.

- ✅ **Kary samplingu OpenAI na CPU i GPU Mojo (2026-07-21).**
  `frequency_penalty`, `presence_penalty`, `repetition_penalty` i okno
  `repeat_last_n` działają w API, samplerze CPU oraz pojedynczej i batchowej
  ścieżce GPU. Historia obejmuje prompt i odpowiedź. Mojo nakłada wszystkie
  kary z histogramu jednym kernelem przed istniejącym równoległym argmax/top-k,
  bez D2H histogramu w trybie release. `logprobs` są liczone po karach.
  Greedy bez kar zachowuje niezmieniony fast path. Mikrobenchmark RTX 4090 dla
  słownika 151936: greedy 6,99 us, top-k 20 z karą 53,73 us, top-k 64 z karą
  160,05 us.

- 🟡 **Natywne Qwen3.5/3.6 MTP/NextN dla gęstego NVFP4 GGUF
  (2026-07-21).** Rejestr `qwen35` oddziela jeden blok
  `nextn_predict_layers` od 64-warstwowego hybrydowego trunku modelu
  `protoLabsAI/ThinkingCap-Qwen3.6-27B-MTP-GGUF`. Loader zachowuje źródłowe
  NVFP4/Q8_0/F32, współdzieli target embedding/head, jeśli checkpoint nie ma
  dedykowanych tensorów, i nie ładuje drugiej kopii targetu. Proposer K=2/K=3,
  batched verifier greedy, argmax oraz checkpointy KV/DeltaNet działają na GPU
  przez kernele Mojo. Retained checkpointy DeltaNet usuwają powtórny skan 48
  warstw przy commit; tryb `--speculative mtp` adaptacyjnie wybiera K=2/K=3.
  Prefill targetu działa w macierzowych chunkach, a catch-up MTP zapisuje tylko
  K/V potrzebne przez draft: raw512 spadł z **227,696 ms do 11,262 ms** przy
  identycznym SHA tokenów. Pula aktywacji każdego modelu hybrydowego wynosi
  1,125 GiB (+128 MiB względem ścieżki attention-only), także bez aktywnego MTP,
  aby batchowy prefill `chunk128` mieścił pełny scratch.
  Wielocyklowy benchmark sprawdza zgodność tokenów z serial greedy. Stała część
  verifiera ma trwałe grafy T=3/T=4 z pozycją bazową odczytywaną na GPU. Wynik
  RTX 4090, K=3: raw128 około **86,9 tok/s**, raw512 około **83,8 tok/s** po
  włączeniu głowy Q8 B3/B4, scalonego przygotowania DeltaNet, szybkich projekcji
  NVFP4 B3/B4 i grafów verifiera.
  llama.cpp osiąga **110,2** i **100,5 tok/s**. Cel
  wydajności nie jest jeszcze osiągnięty. vLLM 0.25.1 nie dostarczył porównywalnego
  wyniku, ponieważ jego loader nie przyjął lokalnego jednoplikowego GGUF jako
  kompletnego repozytorium modelu i zakończył inicjalizację na etapie konfiguracji
  HF/tokenizera. Ograniczenia: greedy, `max_active=1`, zweryfikowane tylko CUDA;
  AMD/Metal pozostają celem przenośności źródeł Mojo, bez testów wykonawczych.
  Raport: `docs/BENCH_QWEN35_MTP_NVFP4.md`.

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

- ✅ **NATIVE-LAYOUT Mojo int8 Q6_K prefill GEMM (down-proj + attn_v) —
  ODZYSKANIE REGRESJI PREFILL (2026-07-20).** Po wycofaniu CUDA MMQ (100 % Mojo)
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
  streaming detok UTF-8, stop-holdback
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
  hybrydowego `qwen35`: K=2/K=3, adaptacyjny wybór budżetu, batched verifier
  i retained checkpointy DeltaNet. Stan targetu i draftu MTP jest izolowany
  per sekwencja pod jednym lease, a strony draftu pochodzą ze współdzielonego
  paged cache MTP. Router `mtp+ngram:2|3` daje
  pierwszeństwo pełnemu draftowi n-gram, dogania MTP po zaakceptowanym prefiksie,
  a na miss używa natywnego MTP; raportuje osobne liczniki obu ścieżek. Wymaga
  greedy; `max_active=2` przechodzi produkcyjny E2E admission/cancel/reuse;
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
    Native MTP nadal przeplata lane seryjnie; pełna ścieżka wymaga batchowego
    draftu oraz verifiera `[B,T]`.
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
  tieringu i arch nie-hybrydowej (`--prefix-cache on|off`, default on). Usage
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
