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
- ❌ **§5.2 Radix-tree prefix caching** (dedup system-promptów/multi-turn) — duże
- ❌ **§5.2 Copy-on-write KV** (beam/n-best), MLA latent cache
- ❌ **§5.4A Expert streaming** (tiering wag MoE, Colibri) — czeka na MoE
- ❌ **§5.4B Trwałe sesje KV** (opt-in persystencja między turami)
- ❌ **§5.3 GDS/cuFile**, hot-swap modeli, **multi-LoRA** (S-LoRA)

### API / serwowanie
- ❌ **§8.1.2 Constrained decoding** (JSON-schema / regex / EBNF grammar mask) — duże
- ❌ **§8.1.2 Prompt caching** jako kontrakt API (cache_control/prompt_cache_key)
- ❌ **§8.1.2** logit_bias, min_tokens, n/best-of, **logprobs**, echo
- ❌ **§8.1 Anthropic API** (/v1/messages), images endpoint
- ❌ **§8.2 FORGE-RPC** (QUIC + CBOR, SDK Rust/Py/TS)
- ❌ **§8.4 Realtime API** (voice-to-voice duplex, barge-in)
- ❌ **§8.5 Batch / offline API** (joby JSONL)

### Produkcja
- ❌ **§9.3 Multi-tenancy**: OIDC/JWT, kwoty/rate-limit, fair-share scheduler,
  izolacja prefix-cache per tenant
- ❌ **§9.4 forge pull** (HF Hub download), **auto-planner**, **forge convert** (kwantyzator)
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
| Constrained decoding (grammar) | średni | JSON/regex/EBNF → automat → maska GPU |
| Radix prefix cache | średni | duży zysk dla multi-turn |
| FORGE-RPC / Realtime / Batch API | średni (każdy) | |
| Spekulacja wpięta w decode | średni | framework już jest |
| forge pull/convert, metryki, multi-tenancy | średni (łącznie) | produkcja |

Wniosek: rdzeń jednokartowego LLM/STT/embeddings jest mocny i produkcyjny,
ale to ~1/3 zakresu spec. Największe brakujące dźwignie wartości: multivendor
HAL, MoE, radix prefix cache, constrained decoding, ONNX. Największe "całe
pillary": multivendor i multi-node.
