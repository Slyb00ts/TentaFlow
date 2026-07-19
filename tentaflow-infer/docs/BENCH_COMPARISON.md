# FORGE vs llama.cpp vs vLLM — Large-Prompt / Large-Output Benchmark

Date: 2026-07-19
GPU: NVIDIA GeForce RTX 4090 (24564 MiB, compute 8.9)
Driver: 610.43.02 (CUDA UMD 13.3), CUDA toolkit (nvcc) 13.3.33
Host: single 4090, ~1.1 GiB baseline VRAM used by the desktop before any engine.

Focus: **large prompt + large output**, single stream (batch = 1), greedy/`temperature=0`.
Every number below comes from a command actually run on this machine; raw tool output is
pasted verbatim in each section. Nothing is estimated or extrapolated.

---

## Engines and exactly what each one ran

| Engine | Version / build | Model | Quant | Notes |
|--------|-----------------|-------|-------|-------|
| **FORGE** | `target_shared/release/forge` (prebuilt, not rebuilt) | `test-models/gguf/mistral-7b-q4_k_m.gguf` | GGUF **Q4_K_M** | `bench --prefix-cache off`; tensor-core f16 GEMM prefill, software Q4_K dequant → f16 |
| **llama.cpp** | git `571d0d5` (master; ggml 0.17.0), `-DGGML_CUDA=ON`, `CMAKE_CUDA_ARCHITECTURES=89` | same `mistral-7b-q4_k_m.gguf` | GGUF **Q4_K_M** (identical weights) | `llama-bench -ngl 99`, 5 reps each |
| **vLLM** | `vllm/vllm-openai:latest` = **0.25.1** (Docker) | `TheBloke/Mistral-7B-Instruct-v0.2-AWQ` | **AWQ 4-bit** (Marlin kernel) | `--enforce-eager --no-enable-prefix-caching`; client-measured TTFT + decode |

**Weight-parity note:** FORGE and llama.cpp run the *exact same GGUF file* → a true
apples-to-apples comparison. vLLM cannot load Q4_K_M; the fairest feasible point is a
different 4-bit Mistral-7B (AWQ). It is 4-bit like Q4_K_M but a **different model
(Instruct-v0.2), a different quant scheme, and different kernels** — treat vLLM as a
"server-grade 4-bit reference point", not a like-for-like swap.

---

## Results

Prefill = tokens/s to ingest the prompt. Decode = tokens/s of generation.
FORGE's decode is measured *after* the prompt is in the KV cache. For llama.cpp the fair
match is `tg @ depth` (generate after a prompt of that depth), shown alongside the depth-0
`tg`. vLLM decode is client-measured steady-state (output_tokens / (total − TTFT)).

| Scenario (prompt / gen) | Engine | Model / quant | Prefill tok/s | Decode tok/s |
|---|---|---|---:|---:|
| **512 / 128**   | FORGE      | Mistral-7B Q4_K_M | **1417** | **171** |
|                 | llama.cpp  | Mistral-7B Q4_K_M | **12789 ± 507** | **183** (tg128@0) |
|                 | vLLM       | Mistral-7B-v0.2 AWQ | **9983** | **131** |
| **4096 / 2048** | FORGE      | Mistral-7B Q4_K_M | **1952–1964** | **144–146** |
|                 | llama.cpp  | Mistral-7B Q4_K_M | **12064 ± 123** | 177@0 / **161** @d4096 |
|                 | vLLM       | Mistral-7B-v0.2 AWQ | **~9621** (9178 / 10064) | **131.5** |
| **8192 / 1024** | FORGE      | Mistral-7B Q4_K_M | **1758** | **129** |
|                 | llama.cpp  | Mistral-7B Q4_K_M | **11019 ± 12** | 180@0 / **149** @d8192 |
|                 | vLLM       | Mistral-7B-v0.2 AWQ | **9423** | **131.8** |

VRAM (single-stream, observed peak incl. ~1.1 GiB desktop baseline):
- **FORGE**: ~23.6 GiB peak — dominated by a pre-reserved full-context KV arena
  (`--kv-pages 0` = full ctx); weights themselves are ~4.1 GiB.
- **llama.cpp**: weights 4.07 GiB + KV for the tested depth (well under 24 GiB; `-ngl 99`).
- **vLLM**: 23.9 GiB — `--gpu-memory-utilization 0.92` deliberately reserves a 143k-token
  KV pool (11.6x concurrency); working set for one request is far smaller.

---

## Honest analysis

1. **Prefill: FORGE is far slower — much worse than the "~2× slower" hypothesis. It is
   ~6–9× slower than llama.cpp** on identical Q4_K_M weights: 1.4–2.0k tok/s vs
   **11–12.8k tok/s**. vLLM (AWQ) also sits at ~9.4–10k tok/s, ~5× FORGE. This is the
   headline result and it is unambiguous.

2. **The cause is NOT "no tensor cores".** I inspected the compiled kernel: FORGE's prefill
   GEMM (`kernels/mojo/src/gemm.mojo`, e.g. `gemm_q4_k_f16`) *does* emit
   `mma.sync.aligned.m16n8k16.row.col.f32.f16.f16.f32` (16 `mma.sync`, 0 scalar `fma.rn.f32`
   in the PTX) — it is a real m16n8k16 f16 tensor-core GEMM. The gap is therefore
   *efficiency*, not absence of tensor cores. Likely contributors: (a) FORGE dequantizes
   Q4_K → f16 and feeds f16 MMA, whereas llama.cpp's MMQ path runs **int8 tensor-core
   matmul directly on the quantized weights**, avoiding the dequant bandwidth; (b) llama.cpp's
   MMQ/cuBLAS prefill tiling is very heavily tuned (occupancy, double-buffered cp.async,
   split-K), while FORGE's BM64 f16 tile is a newer, less-tuned kernel (the project's own
   notes flag GEMM "decomposition work" as unfinished). Net: FORGE leaves most of the 4090's
   tensor-core prefill throughput on the table.

3. **Decode is where FORGE is competitive.** At the fair "decode-after-a-big-prompt" point,
   FORGE does 145 tok/s @4k and 129 tok/s @8k, vs llama.cpp's 161 / 149 (FORGE ~10–15%
   behind) and vLLM's ~131 (**FORGE roughly ties / slightly beats vLLM**). At the small
   512/128 point FORGE decode (171) is close to llama.cpp (183). Single-stream decode is
   memory-bandwidth bound and FORGE's GEMV path is clearly in the right ballpark — it is the
   prefill GEMM, not decode, that needs work.

4. **Where FORGE wins / ties / loses.** Wins: nowhere outright on raw throughput here. Ties:
   decode vs vLLM at batch-1. Loses: prefill against both (badly), and decode slightly to
   llama.cpp. For a large-prompt + large-output workload, prefill is a one-time cost amortized
   over the long generation, so end-to-end FORGE is less penalized than the 6× prefill gap
   suggests — but on very large prompts with short outputs the prefill deficit dominates.

### Fairness caveats (read before quoting these numbers)
- **vLLM is a different model + quant** (Instruct-v0.2, AWQ, Marlin int4 tensor-core kernels)
  — not the same weights as FORGE/llama.cpp. Its numbers are a reference point, not a
  like-for-like result.
- **vLLM ran `--enforce-eager`** (CUDA graphs / torch.compile disabled) because this image's
  graph-capture path crashed on this driver (torch stable-ABI error in `aten::empty`). Eager
  mode slightly *understates* vLLM decode; a graph-captured vLLM would be a bit faster.
- **Methodology differs**: llama.cpp = `llama-bench` internal timing (5 reps, ± stddev);
  vLLM = client-measured TTFT + stream timing; FORGE = its own `bench` timer (prefill timed
  to first visible token, so it includes ≥1 decode step — a small pessimism on FORGE prefill).
- vLLM prefill was measured with prefix caching **off** (to match FORGE's `--prefix-cache
  off`); with caching on, a repeated prompt returned in ~0.02 s (bogus 170k tok/s) — excluded.
- vLLM's own 10 s-window log throughput (`Avg prompt throughput`) averages prefill across a
  window and reads low (290–819 tok/s); the client TTFT figure (~9.4–10k tok/s) is the
  correct instantaneous prefill and is what the table uses.

---

## Raw output

### FORGE (`forge bench ... --prefix-cache off`), re-run on this machine
```
=== 512/128 ===   prefill 512  0.361s  1417.2 tok/s | decode 127  0.741s  171.4 tok/s
=== 4096/2048 === prefill 4096 2.098s  1952.3 tok/s | decode 2047 14.202s 144.1 tok/s
                  (2nd run: prefill 1964.3 | decode 146.2)
=== 8192/1024 === prefill 8192 4.661s  1757.7 tok/s | decode 1023 7.935s  128.9 tok/s
```

### llama.cpp `llama-bench` (build 571d0d5, CUDA, -ngl 99)
```
| model                  | backend | ngl |            test |                  t/s |
| llama 7B Q4_K - Medium | CUDA    |  99 |           pp512 |    12788.80 ± 506.59 |
| llama 7B Q4_K - Medium | CUDA    |  99 |           tg128 |        182.51 ± 0.19 |
| llama 7B Q4_K - Medium | CUDA    |  99 |          pp4096 |    12063.63 ± 122.71 |
| llama 7B Q4_K - Medium | CUDA    |  99 |          tg2048 |        177.24 ± 0.07 |
| llama 7B Q4_K - Medium | CUDA    |  99 |  tg2048 @ d4096 |        161.41 ± 0.03 |
| llama 7B Q4_K - Medium | CUDA    |  99 |          pp8192 |     11019.27 ± 12.45 |
| llama 7B Q4_K - Medium | CUDA    |  99 |          tg1024 |        179.61 ± 0.06 |
| llama 7B Q4_K - Medium | CUDA    |  99 |  tg1024 @ d8192 |        149.43 ± 0.01 |
```

### vLLM 0.25.1 (AWQ, --enforce-eager --no-enable-prefix-caching), client-measured
```
4096/2048 run1: prompt=4101 tok  ttft=0.4468s  prefill=9178.0  decode=131.7 tok/s
4096/2048 run2: prompt=4101 tok  ttft=0.4075s  prefill=10063.8 decode=131.4 tok/s
8192/1024 run1: prompt=8194 tok  ttft=0.8695s  prefill=9423.6  decode=132.0 tok/s
8192/1024 run2: prompt=8194 tok  ttft=0.8696s  prefill=9422.8  decode=131.6 tok/s
512/128       : prompt=521  tok  ttft=0.0522s  prefill=9982.8  decode=130.6 tok/s
```
vLLM engine log (corroborates decode): `Avg generation throughput: 131.5 tokens/s`.

---

## Exact commands

```bash
# FORGE (prebuilt binary — NOT rebuilt)
forge bench mistral-7b-q4_k_m.gguf --prompt-tokens 4096 --tokens 2048 --prefix-cache off

# llama.cpp
git clone https://github.com/ggml-org/llama.cpp          # HEAD = 571d0d5
cmake -B build -DGGML_CUDA=ON -DCMAKE_BUILD_TYPE=Release -DCMAKE_CUDA_ARCHITECTURES=89 -DLLAMA_CURL=OFF
cmake --build build --target llama-bench -j
./llama-bench -m mistral-7b-q4_k_m.gguf -ngl 99 -p 4096 -n 2048
./llama-bench -m mistral-7b-q4_k_m.gguf -ngl 99 -n 2048 -d 4096   # decode after 4096-token prompt

# vLLM (Docker). --privileged -v /dev:/dev works around a nvidia-container-toolkit bug on
# this driver: it created /dev/nvidia-uvm with major 237 instead of the real 511, so
# CUDA init failed ("CUDA unknown error") until the host /dev was bind-mounted.
docker run -d --gpus all --privileged -v /dev:/dev -p 8000:8000 \
  -v ~/.cache/huggingface:/root/.cache/huggingface -e HF_TOKEN=... \
  vllm/vllm-openai:latest --model TheBloke/Mistral-7B-Instruct-v0.2-AWQ \
  --quantization awq_marlin --max-model-len 12288 --gpu-memory-utilization 0.92 \
  --enforce-eager --no-enable-prefix-caching
python3 vllm_client.py TheBloke/Mistral-7B-Instruct-v0.2-AWQ 4096 2048
```

## Setup problems encountered (and how they were handled)
- **Pinned llama.cpp commit `6b80c74f` not reachable** via shallow clone; used current
  master `571d0d5` instead (task permitted either — commit recorded above).
- **vLLM CUDA init failed** ("CUDA unknown error"): the nvidia-container-toolkit created
  `/dev/nvidia-uvm` with the wrong major (237) vs the host's 511. Fixed with
  `--privileged -v /dev:/dev`.
- **vLLM graph-capture crash** (torch stable-ABI `aten::empty` error at CUDA-graph capture
  on this driver): worked around with `--enforce-eager` (documented caveat above).
```
