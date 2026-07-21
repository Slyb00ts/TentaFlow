# FORGE Benchmark Matrix — RTX 4090

**Date:** 2026-07-20
**GPU:** NVIDIA GeForce RTX 4090, 24564 MiB, driver 610.43.02, CUDA 13.3 (nvcc V13.3.33)
**FORGE build:** `target_shared/release/forge` (this working tree)
**llama.cpp (Mistral, Bielik ref-only):** commit `112c781` (`llama-bench`, `llama-batched-bench`, CUDA arch 89)
**llama.cpp (Qwen3.6):** `origin/master` commit `178a6c44` (build `1536`) — required to load the Qwen3.6 hybrid GDN GGUF (see below)
**vLLM:** `vllm/vllm-openai:latest`, version **0.25.1**
**Methodology note:** every number below is copied from a command that was actually run on this box.
Raw logs live in `scratch/bench2/` (outside the repo tree).

---

## Summary table

Single-stream columns: **pp** = prefill tok/s, **tg** = decode tok/s.
Concurrency columns: **aggregate decode tok/s** across N concurrent sequences (fixed prompt ≈512 tok,
128 generated tok/seq). llama.cpp uses `llama-batched-bench` `S_TG t/s`; FORGE and vLLM use a concurrent
OpenAI client firing N simultaneous `/v1/completions` (aggregate output tok/s, prefill deduped by prefix
cache so the figure is decode-dominated — directly comparable to `S_TG`).

| Model | Engine | pp t/s | tg t/s | agg@1 | agg@4 | agg@8 | agg@16 | agg@32 |
|-------|--------|-------:|-------:|------:|------:|------:|-------:|-------:|
| Mistral-7B Q4_K_M | **FORGE** | **11103** | 177 ¹ | 165 | 166 | 166 | 166 | 166 |
| Mistral-7B Q4_K_M | llama.cpp | 7473 | **169** | 176 | **593** | **816** | **1582** | **2562** |
| Bielik-7B NVFP4 | **FORGE** (NVFP4) | 4389 | **130** ² | 122 | 144 | 144 | 144 | 144 |
| Bielik-7B FP8 | vLLM (FP8, comp-tensors) | **14478** | 85 ² | 90 | **354** | **688** | **1311** | **2430** |
| Qwen3.6-35B-A3B | **FORGE** | 50.6 ³ | 44.7 ³ | 45 | OOM⁴ | OOM⁴ | OOM⁴ | OOM⁴ |
| Qwen3.6-35B-A3B | llama.cpp (master) | **7898** ³ | **208** | 204 | **461** | OOM⁵ | OOM⁵ | OOM⁵ |

¹ FORGE Mistral tg measured decode-from-short-context (matches `llama-bench`'s decode-from-empty). At a
  full 4096-token context FORGE decode is **150 t/s**.
² Bielik tg for FORGE **and** vLLM was measured at a 4096-token context (apples-to-apples). FORGE
  decode-from-short = **157 t/s**.
³ Qwen3.6 single-stream prefill was measured at **pp2048** (not 4096): a 4096-token single-shot prefill of
  the 22 GB model OOMs FORGE on 24 GB (FORGE `bench` does not chunk prefill). Both engines use pp2048/tg512.
  FORGE Qwen tg measured at 2048-ctx.
⁴ FORGE `serve` OOMs on Qwen3.6 at **any** setting on 24 GB — a fixed ~150 MB graph allocation cannot fit
  alongside the 22 GB of weights. FORGE Qwen concurrency (N≥4) is infeasible on this card; agg@1 = the
  single-stream decode number.
⁵ llama.cpp Qwen3.6 concurrency is bounded by 24 GB VRAM: 20.74 GiB of weights + the hybrid GDN
  recurrent-state cache (which scales with `-np`) + multi-seq KV do not fit for N≥8. N=1,4 shown; N≥8 OOM.

---

## Raw command outputs

### 1. Mistral-7B Q4_K_M — FORGE vs llama.cpp

**FORGE single-stream** (`forge bench --prompt-tokens 4096 --tokens 512 --prefix-cache off --reps 5`):
```
| phase   | tokens | seconds | tok/s   |
| prefill |   4096 |   0.369 | 11103.2 |
| decode  |    511 |   3.409 |   149.9 |
```
FORGE decode-from-short-context (`--prompt-tokens 64 --tokens 512 --reps 5`): decode **177.0 t/s**.

**llama.cpp single-stream** (`llama-bench -p 4096 -n 512 -ngl 99`, commit 112c781):
```
| llama 7B Q4_K - Medium | 4.07 GiB | 7.25 B | CUDA | 99 | pp4096 | 7473.21 ± 8.01 |
| llama 7B Q4_K - Medium | 4.07 GiB | 7.25 B | CUDA | 99 |  tg512 |  169.09 ± 0.13 |
```

**llama.cpp concurrency** (`llama-batched-bench -c 24576 -npp 512 -ntg 128 -npl 1,4,8,16,32 -np 32`):
```
|   PP |  TG |  B |  N_KV |  T_PP s | S_PP t/s |  T_TG s | S_TG t/s |     T s |   S t/s |
|  512 | 128 |  1 |   640 |   0.048 | 10625.05 |   0.726 |   176.25 |   0.774 |  826.41 |
|  512 | 128 |  4 |  2560 |   0.160 | 12810.97 |   0.863 |   593.05 |   1.023 | 2501.97 |
|  512 | 128 |  8 |  5120 |   0.323 | 12695.26 |   1.254 |   816.36 |   1.577 | 3246.70 |
|  512 | 128 | 16 | 10240 |   0.640 | 12797.64 |   1.294 |  1582.34 |   1.934 | 5293.63 |
|  512 | 128 | 32 | 20480 |   1.270 | 12901.47 |   1.599 |  2561.77 |   2.869 | 7138.81 |
```

**FORGE concurrency** (`forge serve --max-active 32 --batch-min 2 --prefill-chunk 512`, concurrent OpenAI client, prompt 513 tok, gen 128):
```
N=1  agg 165.24 t/s  wall 0.954s
N=4  agg 165.99 t/s  wall 3.085s   (mean req latency 3.079s ≈ wall → all 4 finished together)
N=8  agg 166.11 t/s  wall 6.165s
N=16 agg 166.18 t/s  wall 12.324s
N=32 agg 166.19 t/s  wall 24.647s
```
Aggregate is **flat**: per-sequence throughput falls as 166/N. Reproduced with `--batch-min 12` (default)
and with distinct (non-cached) prompts (N=16 → 89.6 t/s, worse). GPU sat at 100% util throughout.

### 2. Bielik-7B — FORGE (NVFP4) vs vLLM (FP8)

> **QUANT CAVEAT (loud):** FORGE runs **TentaFlow NVFP4** (software FP4 dequant). vLLM cannot load the
> TentaFlow NVFP4 weights, so vLLM runs a **different quant** — `speakleash/Bielik-Minitron-7B-v3.0-Instruct-FP8-Dynamic`
> (compressed-tensors FP8, hardware FP8 tensor cores). These are **not bit-identical weights**; the vLLM
> column is a reference point for the engine, not a like-for-like quant comparison. FP8 also has a hardware
> fast path on Ada that software NVFP4 does not — this flatters vLLM's prefill in particular.

**FORGE single-stream** (`forge bench --prompt-tokens 4096 --tokens 512 --prefix-cache off --reps 5`):
```
| phase   | tokens | seconds | tok/s  |
| prefill |   4096 |   0.933 | 4389.4 |
| decode  |    511 |   3.924 |  130.2 |
```
FORGE decode-from-short-context: **157.0 t/s**.

**vLLM single-stream** (streaming client, max-model-len 8192):
- decode (tg512, from 4096-ctx, `ignore_eos`): **85.0 t/s** (4 reps, all 85.0).
- prefill: automatic prefix caching made repeated prompts a cache hit (TTFT 0.024s → bogus 170k t/s);
  measured with **cache-busting unique prompts**: TTFT ≈0.281s → **pp ≈ 14478 t/s** (4067-tok prompt).

**vLLM concurrency** (concurrent OpenAI client, prompt 511 tok, gen 128, `ignore_eos`):
```
N=1  agg   90.36 t/s  wall 1.417s
N=4  agg  353.60 t/s  wall 1.448s
N=8  agg  687.64 t/s  wall 1.489s
N=16 agg 1310.68 t/s  wall 1.563s
N=32 agg 2430.42 t/s  wall 1.685s
```
Near-linear scaling (~27× at N=32; wall barely grows).

**FORGE concurrency** (`forge serve --max-active 32`, prompt 512 tok, gen 128):
```
N=1  agg 121.65 t/s  wall 1.052s
N=4  agg 143.93 t/s  wall 3.557s
N=8  agg 144.01 t/s  wall 7.111s
N=16 agg 144.07 t/s  wall 14.215s
N=32 agg 143.94 t/s  wall 28.456s
```
Same flat pattern as Mistral — confirms it is architectural, not model-specific.

### 3. Qwen3.6-35B-A3B (hybrid MoE + Gated-DeltaNet) — FORGE vs llama.cpp

**llama.cpp load blocker & resolution.** The local `~/llama.cpp` (commit 112c781) fails to load the GGUF:
```
llama_model_load: error loading model: missing tensor 'blk.40.ssm_conv1d.weight'
```
This is an architecture/graph mismatch for the `qwen35moe` hybrid (name "Qwen3.6 35B A3B"), **not** the
fused-GDN SIGABRT the FORGE patch addresses. **Current llama.cpp master (`178a6c44`, 1536 commits ahead)
loads and runs it** (upstream added qwen3.5/3.6 + gated_delta_net support: PRs #24593, #24025, #23940,
#24581, …). So the Qwen3.6 comparison is **UNBLOCKED** via a freshly built master (`build2`, CUDA arch 89),
no patch needed.

**FORGE single-stream** (`forge bench --prompt-tokens 2048 --tokens 512 --ctx 3072 --prefix-cache off --reps 5`):
```
| phase   | tokens | seconds | tok/s |
| prefill |   2048 |  40.506 |  50.6 |
| decode  |    511 |  11.421 |  44.7 |
```
(4096-token prefill and MoE fp8-KV both fail on FORGE: MoE supports only f16 KV, and a 4096-token
single-shot prefill of the 22 GB model OOMs on 24 GB.)

**llama.cpp single-stream** (`llama-bench -p 2048 -n 512 -ngl 99`, master):
```
| qwen35moe 35B.A3B Q4_K - Medium | 20.74 GiB | 35.51 B | CUDA | 99 | pp2048 | 7898.47 ± 49.44 |
| qwen35moe 35B.A3B Q4_K - Medium | 20.74 GiB | 35.51 B | CUDA | 99 |  tg512 |  207.90 ± 0.30  |
```

**llama.cpp concurrency** (`llama-batched-bench`, master; N≥8 OOMs — 20.74 GiB weights + GDN rs-cache + KV > 24 GB):
```
|  PP |  TG | B | N_KV |  T_PP s | S_PP t/s |  T_TG s | S_TG t/s |    T s |  S t/s |
| 512 | 128 | 1 |  640 |   0.147 |  3476.96 |   0.628 |   203.83 |  0.775 | 825.56 |
| 512 | 128 | 4 | 2560 |   0.545 |  3756.35 |   1.112 |   460.55 |  1.657 |1545.03 |
```
N=8/16/32: `cudaMalloc failed: out of memory ... failed to allocate buffer for rs cache`.

**FORGE concurrency:** `forge serve` OOMs on Qwen3.6 at every setting tried (down to `--max-active 4
--kv-pages 96 --ctx 1024`): `out of device memory: requested 150994944 bytes, available 82879744`. The
serve path's fixed graph buffers do not fit alongside 22 GB of weights on 24 GB. Concurrency infeasible;
agg@1 = single-stream decode 44.7 t/s.

---

## Honest analysis

- **Single-stream, FORGE wins where it is tuned.** On Mistral-7B FORGE prefill (11103 t/s) is **1.49×**
  llama.cpp (7473) and decode is a hair faster like-for-like (177 vs 169 from short context; 150 at a
  4096-ctx). FORGE's fused single-sequence path is genuinely excellent — that is the case it is built for.
- **Concurrency is where FORGE loses badly.** FORGE's server does **not** batch-scale: aggregate decode is
  **flat** at ~166 t/s (Mistral) / ~144 t/s (Bielik) from N=1 to N=32 — per-user throughput just divides by
  N. llama.cpp scales Mistral to **2562 t/s** (14.5×) and vLLM scales Bielik to **2430 t/s** (27×). So under
  concurrency llama.cpp is ~15× and vLLM ~17× FORGE's aggregate. Reproduced across two models, two
  `--batch-min` settings, and cached vs distinct prompts, with the GPU pinned at 100% — this is
  architectural (the batched decode path is not amortizing weight reads across the batch), not a tuning
  artifact. Note: the referenced `batched_bielik.rs` is a **golden-ids correctness** test (B=1 and B=4), not
  a throughput benchmark; it does not demonstrate the "~36×" aggregate scaling, and I could not reproduce
  any aggregate scaling through the server.
- **Bielik vs vLLM (quant-mismatched).** Single-stream, FORGE NVFP4 decode (130 t/s @4096-ctx) is **1.5×**
  vLLM FP8 (85 t/s) — FORGE is the better single-user engine. But vLLM's FP8 hardware path gives it ~3.3×
  FORGE's prefill (14478 vs 4389, software FP4 dequant is the culprit), and vLLM's continuous batching wins
  aggregate throughput outright. **Caveat: different quant (NVFP4 vs FP8-dynamic), so this is an engine
  reference, not a weights-identical result.** No `--enforce-eager` was needed; vLLM ran with CUDA graphs.
- **Qwen3.6-35B MoE is FORGE's weakest showing.** Once llama.cpp master is used (comparison **not** blocked),
  llama prefill is **156×** FORGE's (7898 vs 50.6 t/s) and decode is **4.6×** (208 vs 44.7). FORGE's MoE
  prefill at 50 t/s indicates an essentially unoptimized expert path. Both engines are then wedged by the
  24 GB card for concurrency: llama fits only N≤4 (461 t/s aggregate at N=4), and FORGE's `serve` cannot
  even initialize the 22 GB model (fixed graph overhead OOMs). So Qwen3.6 concurrency is a VRAM story for
  both, but FORGE's single-stream MoE numbers are far behind.
- **Fairness caveats, collected:** (1) Bielik is a quant mismatch (NVFP4 vs FP8). (2) Concurrency metric is
  aggregate decode tok/s; llama uses `S_TG` (decode-only), FORGE/vLLM use client wall time with prefix
  caching on identical prompts (decode-dominated) — comparable but not identical instrumentation. (3) tg
  context length differs by pair (footnotes ¹²³); values are matched within each pair where possible.
  (4) Qwen uses pp2048 not pp4096 because 4096 OOMs FORGE. (5) llama-bench is warm by design; FORGE uses
  `--reps 5` warm best-of-N (rep 1 discarded). (6) No `--enforce-eager` was required anywhere.
