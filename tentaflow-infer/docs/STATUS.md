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
  (MoE). Brak: DeepSeek (MLA), Gemma (sliding-window), Phi.
- 🟡 **§4.2 qwen35moe (Qwen3.6-35B-A3B hybrid SSM+MoE)** — rozpoczęte:
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
  - ❌ **Kernele Mojo**: hd256 flash-attention (decode split + prefill + combine,
    obecnie tylko hd64/hd128), depthwise conv1d_k4, recurrent Gated-DeltaNet scan,
    gated-RMSNorm, partial M-RoPE dla hd256. Wymaga PTX rebuild + launchery + golden.
  - ❌ **Stan SSM w KV** (`SeqKv`): rezydentny bufor stanu `[n_v_heads, d_state,
    d_state]` + `[conv_dim, d_conv-1]` per warstwa DeltaNet, obok paged KV dla
    warstw atencji.
  - ❌ **Forward hybrydowy w silniku**: dispatch per-`LayerKind`, bramkowana
    atencja hd256, ścieżka DeltaNet (conv+scan+gated norm), MoE z shared expert
    w tej ścieżce, prefill sekwencyjny + decode. Cel bramki: spójny tekst
    zgodny z llama.cpp (~194 tok/s referencyjnie na RTX 4090).
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
  prefill batched. TODO (perf): grouped-GEMM permute/unpermute zamiast pętli,
  batched-MoE decode, KV low-bit/tiering dla MoE.
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
