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

- 🟡 **§6 Spekulacja**: framework (Proposer trait, NgramProposer, kaskada,
  adaptive-disable) ISTNIEJE, ale NIE jest wpięty w pętlę decode — brak draft
  model / MTP / EAGLE proposerów i tree-verification w silniku. To rusztowanie,
  nie działająca akceleracja.
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
  - 🟡 **Wydajność ścieżki hybrydowej**: korektność najpierw — host round-tripy
    per warstwa (gather embed, bramka atencji, router MoE), brak grafu CUDA i
    wsadu, DeltaNet skanowany per token wieloma małymi `device.copy`. ~17 tok/s
    (llama.cpp ~194). Optymalizacja (kernel sigmoid-mul bramki, wsadowy prefill,
    graf decode, mniej sync-ów) to follow-up.
- 🟡 **§9.2 Odporność**: admission ✅; brak respawn workera po crashu, health
  per-GPU, pełnego graceful drain.
- 🟡 **§8.3 Operacyjność**: /healthz ✅; brak metryk Prometheus/OTel, hot reload.
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
  per-token pętla ekspertów przez indeksowane quant-GEMV (offset bajtowy w
  stackowanym tensorze `ffn_*_exps`), akumulacja `moe_scale_add`. Wspiera
  full-vector QK-norm (OLMoE) i per-head (qwen3moe), shared experts (design).
  Decode single-stream (bez CUDA-graph — wybór ekspertów zależny od danych) +
  prefill batched. KV-tiering: ✅ dla hybrydy qwen35moe (warstwy atencji), ❌ dla
  nie-hybrydowego MoE (brak staged-attention decode). TODO (perf): grouped-GEMM
  permute/unpermute zamiast pętli, batched-MoE decode, KV low-bit dla MoE.
- ❌ **§4.4 MLA** (DeepSeek), **sliding-window + sinks**, **linear/SSM** (Mamba)
- ❌ **ONNX** (import grafu → IR; parakeet/silero/depth z .runtime)

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
- ❌ **§5.4B Trwałe sesje KV** (opt-in persystencja między turami)
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
- ❌ **§8.1 Anthropic API** (/v1/messages), images endpoint
- ❌ **§8.2 FORGE-RPC** (QUIC + CBOR, SDK Rust/Py/TS)
- ❌ **§8.4 Realtime API** (voice-to-voice duplex, barge-in)
- ❌ **§8.5 Batch / offline API** (joby JSONL)

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

| Obszar | Rozmiar | Uwaga |
|---|---|---|
| Multivendor HAL (ROCm/Metal/Intel) | bardzo duży | rdzeń "uniwersalności"; osobny backend + rekompilacja kerneli Mojo per target |
| Multi-node TP/PP/EP + ForgeCCL | bardzo duży | cały pillar §7 |
| Graph IR + kompilator + autotuner | duży | zamienia ręczny forward |
| MoE (kernele + expert streaming) | duży | odblokowuje DeepSeek/Mixtral/Qwen-MoE |
| Modalności TTS/T2I/Video | duży (każda) | osobne silniki |
| ONNX loader | duży | import opset 17+ subset |
| Radix prefix cache | średni | duży zysk dla multi-turn |
| FORGE-RPC / Realtime / Batch API | średni (każdy) | |
| Spekulacja wpięta w decode | średni | framework już jest |
| forge pull/convert, metryki, multi-tenancy | średni (łącznie) | produkcja |

Wniosek: rdzeń jednokartowego LLM/STT/embeddings jest mocny i produkcyjny,
ale to ~1/3 zakresu spec. Największe brakujące dźwignie wartości: multivendor
HAL, MoE, radix prefix cache, ONNX. Największe "całe
pillary": multivendor i multi-node.
