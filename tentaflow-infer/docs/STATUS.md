# FORGE — Status realizacji vs SPEC

Uczciwa inwentaryzacja tego, co jest zrobione, częściowe i nietknięte, mapowana
na sekcje `docs/SPEC.md`. **Reguła utrzymania: aktualizuj ten plik gdy domykasz
lub zaczynasz element (w tym samym commicie).**

Skala: SPEC to plan na ~30-45 inż. × 14 mies. (7 streamów). Zrobiony jest
najtrudniejszy RDZEŃ jednokartowy (kernele, silnik, KV, batching, kwantyzacja)
— produkcyjnej jakości, bramkowany testami. Poniżej reszta.

Legenda: ✅ zrobione · 🟡 częściowe · ❌ nietknięte

Ostatnia aktualizacja: 2026-07-18.

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

- ✅ **§6 Spekulacja (linear n-gram) WPIĘTA w decode loop**: NgramProposer
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
  dzisiejsza pętla). Braki: draft-model / MTP / EAGLE proposery, tree-verification
  (spec-sampling) i spekulacja stochastyczna (temp>0) — na razie tylko greedy-exact.
- 🟡 **§4.2 Rejestr architektur**: qwen3, llama, mistral, olmoe (MoE), qwen3moe
  (MoE), qwen35moe (hybrid SSM+MoE, ✅ E2E — patrz niżej). Brak: DeepSeek (MLA),
  Gemma (sliding-window), Phi.
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
  - ✅ **Stan SSM w silniku** (`Model.ssm`): rezydentny bufor stanu
    `[n_v_heads, d_state, d_state]` f32 + okno conv `[conv_dim, d_conv-1]` f16 per
    warstwa DeltaNet, alokowany raz (pula Weights), zerowany na starcie sekwencji
    (`pos==0`). Persistent/nie-paged (jedna aktywna sekwencja SSM naraz — zgodnie
    z jednostrumieniową ścieżką MoE decode). Warstwy atencji używają paged KV.
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
  własnych prefill-stron po zakończeniu; LRU eviction refcount-0 liści; admission
  liczy trafienie i strony odzyskiwalne. Aktywny dla verbatim `f16`/`fp8` bez
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
