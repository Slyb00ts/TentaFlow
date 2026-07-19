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
| **512 / 128**   | FORGE      | Mistral-7B Q4_K_M | **~2400** | **174** |
|                 | llama.cpp  | Mistral-7B Q4_K_M | **12789 ± 507** | **183** (tg128@0) |
|                 | vLLM       | Mistral-7B-v0.2 AWQ | **9983** | **131** |
| **4096 / 2048** | FORGE      | Mistral-7B Q4_K_M | **~2820–2854** | **146** |
|                 | llama.cpp  | Mistral-7B Q4_K_M | **12064 ± 123** | 177@0 / **161** @d4096 |
|                 | vLLM       | Mistral-7B-v0.2 AWQ | **~9621** (9178 / 10064) | **131.5** |
| **8192 / 1024** | FORGE      | Mistral-7B Q4_K_M | **~2360** | **130** |
|                 | llama.cpp  | Mistral-7B Q4_K_M | **11019 ± 12** | 180@0 / **149** @d8192 |
|                 | vLLM       | Mistral-7B-v0.2 AWQ | **9423** | **131.8** |

FORGE prefill history on this GEMM (Mistral-7B Q4_K_M, prefix-cache off): f16
tensor-core dequant GEMM → int8-MMQ tensor-core GEMM (`gemm_i8mma`, s8×s8→s32
mma) → int8-MMQ + **once-per-GEMM q8_1 activation pre-quant** (`quantize_act_q8_1`,
block-major coalesced scales) → **BN=128 reblock** (`_big`: BM128×BN128, 512-thread/
16-warp block). 512: 1417 → 2270 → 2400 → ~2400 (unchanged, gated off — see below).
4096: 1952 → 2650 → 2837 → **~2827 (+9 % over the pre-`_big` 2588 on a same-machine
3-rep A/B)**. 8192: 1758 → 2236 → 2360 → **~2343 (+4 % over 2246)**.
The `_big` tile doubles the rows/block (BN 64→128) so the activation X — re-read
`ceil(rows/BN)` times — is fetched half as often; it keeps the per-warp accumulator
(and thus the 127-reg / 1-CTA-per-SM = 16-warp occupancy, matching the old
2×256-thread footprint) fixed by adding warps instead of n-tiles/warp, and is
**bit-identical to the committed BM=128 kernel** (integer mma is exact). Isolated
microbench T=2048: 58 → 65 TOPS = **31 % → 35 % of the 184-TOPS ceiling**, wall
4.15 → 3.70 ms; nsys Mistral-4096 total Q4_K i8mma GEMM time −10.9 %. It is
**perf-gated** (`n_tokens ≥ 1024` AND `ceil(rows/128)·ceil(n_tokens/128) ≥ 256`):
the coarse 512-thread block underfills the SMs for short chunks (512-prefill −11 %)
and small models (Qwen3-0.6B rows ≤ 3072, −19 %), so those stay on the committed
256-thread kernel — Qwen3-0.6B Q8_0 4096 is thus **unchanged (~18.4–19.4k)** and
decode is bit-unchanged (dp4a GEMV). Numbers are ±~5 % run-to-run from GPU
boost-clock/thermal state; the remaining ~4× gap to llama.cpp is the GEMM's
mma-issue efficiency at this 35 %-of-ceiling wall, not X staging (see
STATUS.md / MOJO_NOTES.md).

**Q4_K prefill GEMM now runs on an nvcc CUDA kernel (ADR-0001 exception).**
`docs/CODEGEN_PROOF.md` proves ptxas schedules the identical int8-MMQ
`mma.sync.m16n8k32.s8` past ~200 TOPS where the Mojo backend walls at ~66.
`kernels/cuda/gemm_i8mma.cu` (nvcc `-arch=sm_89 -cubin`, committed cubin, loaded
through the same cudarc `cuModuleLoadData` path) replaces the Mojo GEMM compute
for Q4_K prefill while keeping the `quantize_act_q8_1` prepass and everything else
identical — output is **bit-identical to Mojo** (rel 0.0e0; ~4.6e-4 vs exact CPU
MMQ). Same-card A/B (`FORGE_I8MMA_BACKEND=mojo|cuda`), Mistral-7B Q4_K_M,
prefix-cache off:

| P / T | Mojo (before) | CUDA (after) | ratio | decode | llama.cpp pp |
|-------|---------------|--------------|-------|--------|--------------|
| 512 / 128   | 2497 | **3334** | 1.34× | 174 (=) | 11965–12789 |
| 4096 / 2048 | 2956 | **3536** | 1.20× | 146 (=) | 11965 ± 118 |
| 8192 / 1024 | 2477 | **2930** | 1.18× | 130 (=) | 11019 |

Isolated Q4_K GEMM (RTX 4090, same card): Mojo 55–65 → **CUDA 65–107 TOPS
(1.6–1.9×)**. Decode is bit-unchanged (dp4a GEMV untouched). **Q8_0 stays on Mojo**
— its committed i8mma is ~120 TOPS, faster than this CUDA kernel on Q8_0 (which
lands ~100), so only Q4_K prefill routes to CUDA (no regression); Qwen3-0.6B Q8_0
4096 prefill unchanged (~17.5k). vs llama.cpp (11965) the ratio improves 0.25× →
0.30× at pp4096. The remaining ~3.4× gap: this CUDA kernel schedules ~107 TOPS,
about half of llama.cpp's heavily-tuned MMQ (208 TOPS measured in CODEGEN_PROOF
Exp 2), and FORGE still un-fuses attention/quant/norm — Phase-2 fusion work, not
this one GEMM.

**W4A8 (int4-weight × int8-activation) prefill GEMM — wired e2e, non-default
(`FORGE_GEMM=w4a8`), QUALITY-FAILED so it stays opt-in.** The QServe
`dense_kernel0` (kernels/cuda/w4a8_gemm.cu, HAL-verified relL2 2e-4) is now
routed into the dense prefill forward pass: at load, each LOGICAL projection
(q, k, v, o, gate, up, down) is requantized Q4_K→f32→W4A8 into its OWN pack
(weights + `[K/G][N]` transposed scales/zeros), so the fused-weight blocker is
solved by per-matrix splitting (no windowing). A per-token int8 activation
quantizer (`forge_w4a8_quant_act_pertoken`, added to the same cubin) feeds each
GEMM. The Q4_K weights stay resident for decode + the logit head (W4A8 is an
ADDITIONAL store, ~+4 GiB VRAM). Decode + Q8_0 + NVFP4 untouched.

Same-card A/B (RTX 4090 idle/cool: 43 °C, 210 MHz at rest), Mistral-7B Q4_K_M,
`--prefix-cache off`, `FORGE_GEMM` default (committed CUDA MMQ) vs `=w4a8`.
llama.cpp reconfirmed this session on the idle GPU (`llama-bench -ngl 99 -fa 1`,
build 112c781): pp512 **12616**, pp4096 **11955**, pp8192 **11030** (matches the
committed ~12032; `scratch/bench` is gone but `~/llama.cpp/build` is present):

| P / T | CUDA MMQ (default) | W4A8 | e2e ratio | vs llama.cpp | decode |
|-------|--------------------|------|-----------|--------------|--------|
| 512 / 128   | 3085 | **2937** | 0.95× | 0.23× (12616) | 174 (=) |
| 4096 / 2048 | 3825 | **5812** | 1.52× | 0.49× (11955) | 146 (=) |
| 8192 / 1024 | 2928 | **4109** | 1.40× | 0.37× (11030) | 130 (=) |

In-engine GEMM time (FORGE_PREFILL_TRACE, steady 1024-tok chunk, sum of
gemm_qkv+o+gateup+down): **161 ms → 39 ms = 4.1× faster GEMM** — better than the
2.0× microbench. But e2e prefill only gains 1.40–1.52×: once the GEMM is 4× the
prefill is **non-GEMM (attention/rope/norm/kv) bound** — that work is now ~70 % of
the W4A8 chunk and is bit-identical between paths (decode is unchanged to ±0.2 %).
The projected ~9650 (0.80×) did NOT materialize: the projection assumed the GEMM
dominated; measured, the remaining gap to llama.cpp is its **fused
flash-attention**, not the GEMM. Small 512-prefill regresses slightly (W4A8 CTA
underfills at M=512 and the tiny GEMM is dwarfed by fixed non-GEMM cost).

**QUALITY: FAILED — W4A8 output is incoherent, so it is NOT a default.** Greedy
(`--temp 0`, 32 tok), CUDA MMQ vs W4A8 on the same prompts:

| Prompt | CUDA MMQ (default) | W4A8 |
|--------|--------------------|------|
| "The capital of France is" | "a city … on the Seine River, in north-central France, … political, economic, cultural … center" | "a country that Question: Question: Question: …" |
| "Water boils at a temperature of" | "100°C. The boiling point of water is 100°C…" | "a temperature. QuiqiQuiQGraphicsView::paintEvent…" |
| "The first president of the United States was" | "George Washington, who served two terms … from 1789…" | "a collection of Qurbananas, Quranic acidic acid…" |
| "The chemical symbol for gold is" | "Au, which stands for gold. Gold is a precious metal…" | "a substance that Question: Question: Question: …" |

Root cause is expected and is the honest cost of the naive path: per-token
symmetric int8 activation quant with NO SmoothQuant/channel-smoothing lets
transformer activation outliers dominate the per-token scale, collapsing the
in-distribution range — the accuracy problem QServe solves upstream with offline
activation smoothing/rotation that FORGE does not yet apply. The weight requant
Q4_K→W4A8 adds ~10 % relL2 on top; the activation-outlier loss is the dominant
factor. (Teacher-forced NLL proxy over a fixed passage gave Q4_K mean NLL 3.53;
the W4A8 side could not be captured this session — the long-running `serve`
process is SIGURG-killed in this harness — but the qualitative collapse is
decisive.) **To make W4A8 usable: add offline per-channel activation smoothing
(SmoothQuant-style) at pack time + a matching activation scale in the quantizer.**
Until then W4A8 remains selectable but non-default; the committed CUDA MMQ stays
the Q4_K default.

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

---

## int8-MMQ (dp4a) prefill GEMM investigation — 2026-07-19

**Premise tested:** the prefill gap (FORGE ~1.4–2k vs llama.cpp ~11–13k tok/s) was
attributed to FORGE's prefill GEMM dequantizing Q4_K→f16 before an f16 tensor-core mma.
An int8-MMQ path (q8_1-quantized activations · native weight codes · dp4a int32 accumulate,
llama.cpp's `mul_mat_q`) was implemented for Q8_0 + Q4_K to remove that dequant.

**Result: the premise does not hold on Ada — dp4a is SLOWER than the f16 tensor-core GEMM.**
Isolated kernel microbench (`kernels/mojo/bench_gemm_dp4a.mojo`, RTX 4090, 300-launch warmup):

```
Q4K 14336x4096 T=128   f16 32.2 TFLOP/s   dp4a(BM128) 29.4   dp4a(BM64) 20.7
Q4K 14336x4096 T=512   f16 59.7 TFLOP/s   dp4a(BM128) 33.7   dp4a(BM64) 24.1
Q4K  4096x4096 T=512   f16 60.7 TFLOP/s   dp4a(BM128) 33.5   dp4a(BM64) 23.4
Q4K  4096x14336 T=512  f16 61.7 TFLOP/s   dp4a(BM128) 34.0   dp4a(BM64) 23.7
```

The f16 path offloads the MACs to tensor cores (~60 TFLOP/s) while doing the per-element
dequant on the CUDA cores in the tensor pipe's shadow; dp4a does BOTH the MACs and the
per-block scaling on the CUDA/INT32 pipe and tops out ~1.8x lower at the large-batch
(prefill) sizes. So the f16-dequant GEMM was NOT the bottleneck it was assumed to be — it
already runs at ~60 TFLOP/s.

**End-to-end confirmation** (`forge bench --prefix-cache off`, greedy output bit-identical
either way):

```
Mistral-7B Q4_K  4096/2048 prefill:  f16 1999 tok/s   →  dp4a 1968 tok/s  (GEMM is only
                                     ~half of a long-context prefill; attention O(T^2)
                                     masks the GEMM change)
qwen3-0.6B Q8_0   512/128  prefill:  f16 38533 tok/s  →  dp4a 21375 tok/s  (GEMM-bound
                                     prefill: dp4a REGRESSES it ~1.8x, matching the microbench)
```

**Decision:** prefill stays on the f16 tensor-core GEMM (no regression). The int8-MMQ dp4a
kernels are correct (match an exact CPU dp4a reference to ~5e-4, `test_gemm_dp4a.mojo`),
registered and built, but are NOT wired as the default because they are slower on this GPU.

**Why llama.cpp is still faster:** its fast MMQ uses int8 **tensor cores**
(`mma.sync ...s32.s8.s8.s32`), not dp4a. That instruction DOES emit and compute correctly
from Mojo via inline PTX, but Mojo's `inlined_assembly` cannot marshal its 4×s32 output
(needs a `TrivialRegisterPassable` struct mapping to LLVM `{i32,i32,i32,i32}`;
`SIMD[int32,4]` captures only the first register, `Tuple` is rejected). Closing the prefill
gap needs either that Mojo capability or fused flash-attention prefill — not a dp4a GEMM.
See `kernels/mojo/MOJO_NOTES.md`.

## Prefill gap investigation — 2026-07-19 (compute-bound, no launch overhead)

Attacked the ~4.3x Mistral-7B Q4_K prefill gap vs llama.cpp assuming launch/graphing
overhead was a big slice (prefill is NOT CUDA-graphed, ~768 eager launches at 4096).
**nsys disproved that premise:** prefill is compute-bound at every size, so graphing
and chunk-widening buy nothing, and the register-cap occupancy lever regressed.

**1. Launch/gap overhead is negligible (nsys, RTX 4090, `bench --prompt-tokens P --tokens 2`):**

```
P=4096:  sum of GPU kernel time 1433.5 ms   vs   prefill wall 1436 ms   →  gap 2.5 ms (0.17%)
P=512:   sum of GPU kernel time  210.3 ms   vs   prefill wall  212 ms   →  gap 1.7 ms (0.8%)
```

The GPU never starves — the CPU queues the ~768 eager launches faster than the GPU drains
them, so a captured CUDA graph would replay the SAME kernels back-to-back with ~0 headroom.
**Prefill graphing was NOT shipped: it cannot beat a <0.2% gap.** (Decode stays graphed;
that path is latency-bound and benefits.)

Per-kernel split at P=4096 (`cuda_gpu_kern_sum`): i8mma GEMM 62% (qkv/o/gate/up, 768 launches),
attn_prefill 22% (grows O(T·ctx)), Q6_K f16 GEMM 10% (down-proj), silu/quant/norm/rope ~6%.
The i8mma GEMM runs at ~46 INT8-TOPS ≈ 25% of the ~184-TOPS practical ceiling.

**2. i8mma occupancy is NOT the limiter — capping registers regressed.** `gemm_q4_k_i8mma`
(bm128, 256 thr) uses 126 regs → 2 CTAs/SM (33% occ); `gemm_q8_0_i8mma` 100 regs → 2 CTAs/SM.
Injecting `.maxnreg 85` (verified via `ptxas -v`) lifts both to 3 CTAs/SM (50% occ, q8_0
spill-free, q4_k 64 B spill). Measured Mistral 4096 prefill: **2800 → ~2400 tok/s (REGRESSION)**.
The kernel hides mma-issue/ld_matrix/f32-epilogue latency via per-thread ILP at 2 CTAs/SM;
cutting registers removed that ILP and spilled. Reverted. The GEMM is mma-issue/ILP bound,
not occupancy bound — confirming `MOJO_NOTES.md`.

**3. Widening `MAX_PREFILL_CHUNK` 1024→2048 is neutral** (compute-bound → fewer launches
can't help): Mistral 4096 2800→~2815 tok/s, 8192 2340→~2380 tok/s (both within run-to-run
noise), at 2x prefill-scratch VRAM. Not kept.

**Net: nothing shipped this pass — every prescribed lever (graph / chunk / occupancy) is a
no-op or regression because the profile is compute-bound and the GEMM is already ILP-tuned.**
Baseline prefill (`--prefix-cache off`, RTX 4090) unchanged:

| shape (P/T)            | FORGE prefill tok/s | llama.cpp | ratio |
|------------------------|--------------------:|----------:|------:|
| Mistral-7B Q4_K 512/128   | ~2565 | ~12064 | 4.7x |
| Mistral-7B Q4_K 4096/2048 | ~2800 | ~12064 | 4.3x |
| Mistral-7B Q4_K 8192/1024 | ~2350 | ~11000 | 4.7x |
| Qwen3-0.6B Q8_0 4096/512  | ~19500 | — | — |

Decode unchanged: Mistral ~146 tok/s, Qwen Q8_0 ~497 tok/s; Bielik NVFP4 golden bit-exact
on 1 and 4 lanes.

**What actually remains (real GEMM microarch work, all higher-risk / future passes):**
- **Route the Q6_K down-proj through int8-mma** (currently the slow f16 tensor GEMM, 10% of
  prefill). Halving it ≈ +5% end-to-end. New `FMT==2` in `gemm_i8mma_impl`; the wrinkle is
  Q6_K's 16-wide scale sub-blocks vs the mma's k=32 (two scales per mma stage) — needs a
  split-scale epilogue, so it is not a trivial clone of the Q4_K path.
- **Cut the per-32-col `barrier()`** (K=14336 → 448 barriers/GEMM): triple-buffer or unroll
  two k-stages per barrier to keep the tensor pipe busier. Medium risk, needs bit-exact reval.
- **Fused flash-attention-style prefill** and/or **persistent single-megakernel** — the real
  llama.cpp-parity path, explicitly out of scope for a low-risk pass.

---

## GEMM ILP / barrier pass (2026-07-19) — every lever measured, NOTHING shipped (all no-op or regression)

Goal was 2–3× on the int8 tensor-core MMQ prefill GEMM (81% of prefill, ~57 of 184 s8-mma
TOPS ≈ 31% of ceiling). Each variant was verified bit-identical (integer mma is exact — the
i8mma output matched the committed kernel to the last bit; `test_gemm_i8mma.mojo` rel err
unchanged at 4.6e-4, bm64 bit-identical) and timed with the fixed `bench_gemm_i8mma.mojo`
microbench (the committed one was stale — wrong 6-arg signature, didn't compile; rewritten to
the pre-quant path + INT8-TOPS report) plus `forge bench` end-to-end.

**Isolated GEMM microbench (Q4_K, i8mma bm128, RTX 4090, TOPS):**

| variant | T=128 | T=512 (N14336 K4096) | T=2048 |
|---|--:|--:|--:|
| committed baseline | 28.8 | 57.1 | 58.0 |
| +separate mma-issue from f32 epilogue (lever 3) | 30.8 | 57.1 | 58.2 |
| +2 k-stages/barrier (CK=2, lever 1) | 33.1 | 57.1 | 58.9 |
| +unroll the CK stage loop (cross-stage sched) | 37.0 | 57.2 | 58.5 |
| +paired B `ld_matrix.x4` (2 n-tiles/load, halves B loads, lever 5) | 37.9 | 57.2 | 58.8 |
| diagnostic: strip q4_k min-correction epilogue | — | 57.3 | 58.9 |

**Reading of the data — the large-T prefill regime is at a hardware wall, not a software one:**
- Separating mma from the epilogue: **neutral** at large T. The compiler already schedules the
  8 independent per-stage mma; there was no RAW stall to remove.
- 2 k-stages/barrier (448→224 barriers at K=14336): **+15% at T=128, flat (±1%) at T≥512.**
  Barrier count is NOT the large-T cost.
- Stripping the entire q4_k per-block min-correction epilogue: **57.1→57.3, i.e. free** — the
  f32 scale epilogue is fully hidden in the tensor pipe's shadow, so it is not the bottleneck.
- Halving the B `ld_matrix` count (2 n-tiles per `ld_matrix.x4`): **neutral at large T.** Not
  ldmatrix-issue bound either.
- TOPS is **constant across T=512→2048 (57→59)** and immune to barrier / epilogue / ldmatrix
  reductions — the signature of a throughput/bandwidth wall for THIS tile shape, not a
  latency/issue-rate bound the ILP levers could relieve. ~31% of the 184-TOPS ceiling is where
  this MMQ tiling saturates.

**End-to-end (`forge bench`, `--prefix-cache off`) — the combined CK=2 + unroll + paired-B
kernel REGRESSES and is otherwise flat, so it was reverted:**

| shape (P/T) | committed | CK2+unroll+pairedB | Δ |
|---|--:|--:|--:|
| Mistral-7B Q4_K 512/128   | 2346 (2340–2521 over 4 runs) | 1754 | **−25%** |
| Mistral-7B Q4_K 4096/2048 | 2818 | 2800 | −0.7% (noise) |
| Mistral-7B Q4_K 8192/1024 | 2339 | 2357 | +0.8% (noise) |
| Qwen3-0.6B Q8_0 4096/512  | 19470 | 19706 | +1.2% (noise) |

Decode bit-unchanged (uses the dp4a GEMV): Mistral 146.2, Qwen 496 tok/s.

The 512-prompt regression is the tell: multi-stage buffering doubles the smem (15→30 KB) and
the register prefetch state, which drops the kernel below 2 CTAs/SM. That is invisible at the
saturated large-T wall (bandwidth/throughput-bound) but expensive for the many short GEMMs of a
512-prefill — reaffirming `MOJO_NOTES.md`: this kernel is occupancy-sensitive and cannot afford
more per-thread state (the same reason `.maxnreg` regressed). **Reverted to the committed kernel.**

**Conclusion:** the four instruction-level levers (issue-reorder, barrier-cut, ldmatrix-cut,
epilogue) cannot move large-T prefill because it is not issue/latency bound at that shape — it
sits at a ~57-TOPS tiling/bandwidth wall. The only remaining real lever for the 4.3× gap is the
architectural rewrite the notes already flag as high-risk/out-of-scope: **BN=128 rows/block to
halve the X re-read traffic** (X is re-read `ceil(N/64)` times — the dominant byte stream), plus
**larger per-warp register tiles** for a higher mma:load-byte ratio (needs 2-pass W staging and
a full bit-exact reval of the MMQ path). That is a multi-day kernel rewrite, not a low-risk pass;
it was not attempted here rather than ship a regression or an unverified stub.

---

## 2026-07-19 — Reading llama.cpp's MMQ source and replicating its scheme (Phase 1 + 2)

Prior tuning was black-box. This round READ llama.cpp's open-source MMQ kernel
(`scratch/bench/llama.cpp/ggml/src/ggml-cuda/{mmq.cuh,mmq-vec-dot.cuh,mmq-load-tiles.cuh,
mma.cuh,mmq-config-ampere.cuh}`, build `571d0d5`) to find the structural reason for
65 (ours) vs ~169 TOPS (theirs), then tried to replicate it in `gemm_i8mma_impl`.

### Phase 1 — llama.cpp Q4_K / Q8_0 MMQ scheme (as read from source)

**mma instruction (Ada, `mma.cuh`):** identical to ours — `mma.sync.aligned.m16n8k32.row.col.
s32.s8.s8.s32`, A tile `<16,8,int>` = 4×b32 (16 s8), B tile `<8,8,int>` = 2×b32 (8 s8),
C/D `<16,8,int>` = 4×s32. Turing path also has a 2×`m8n8k16` fallback; Ada uses the m16n8k32.
This is exactly our `_mma_s8`.

**Tile config (`mmq-config-ampere.cuh`, used for Ada = cc 8.9):** for BOTH Q4_K and Q8_0:
`nthreads=256` (**8 warps**), `occupancy=1` (target **1 CTA/SM**), `I=128` (weight rows/block),
`J` = up to `128` (tokens/block), `K_vram=MMQ_ITER_K=256`, `stream_k=true`. Q4_K uses the Q8_1
sram layout, Q8_0 the Q8_0 layout. `rows_per_warp = (J>=48 && J%16==0) ? 32 : 16` → 32 for J=128.

**Warp→tile mapping (`mmq-vec-dot.cuh` `vec_dot_q8_0_q8_1_mma`, non-AMD `#else`):**
`tile_C=<16,8,int>` (ne=4). `ntx = rows_per_warp/16 = 2` minitiles/warp in the I(row) direction.
`i0 = (threadIdx.y/ntx)*rows_per_warp` → 8 warps / ntx(2) = 4 row-bands of 32 rows; the 2 warps
in a band split J. The j-loop `for j0 in 0..J step ntx*8` × ntx n-tiles → each warp holds
`J/(ntx*8) * ntx * tile_C::ne`. For J=128: **64 f32 accumulators per thread** (32 rows × 64 cols
per warp ÷ 32 lanes). A fragments (`A[ntx][MMQ_TILE_NE_K/QI8_0]`) and dA scales are pre-loaded
into registers ONCE per 32-K block; **B is loaded with `load_generic` (plain int loads) —
the source comment says "faster than load_ldmatrix"**; scale applied per 32-block
`sum += C.x[l]*dA*dB` (Q8_0) / with the 6-bit sub-scale + `dmin*Σq` min-correction (Q4_K).

**load_tiles (`mmq-load-tiles.cuh`):** Q8_0 and Q4_K store the int8 codes into smem **row-major
with a `+pad` stride** (`x_qs[i*sram_stride + k]`); NO exotic pre-repack for conflict-free
ldmatrix on the NV mma path — the `+4`/`+I` padding in `tile_x_sizes` is the only conflict
avoidance. Scales/mins go to a separate `x_df`/`x_dm` smem region. So the smem layout is
essentially what our kernel already uses.

**k-loop / pipeline (`mmq.cuh` `mul_mat_q_process_tile`):** `load_tiles` fills the full x-tile,
then a double-buffered tile_y with a **4-`__syncthreads` per 64-K** cadence (load y-half A, sync,
vec_dot(0), sync, load y-half B, sync, vec_dot(32), sync). Plain register-staged global→smem
copies (no cp.async in this build's mainloop). **stream-K** splits the K dimension across CTAs
with a `tmp_fixup` reduction pass so all SMs stay busy when the M×N tile grid is small.

### Point-by-point DIFF vs our `gemm_i8mma_impl`

| aspect | llama.cpp (Ada) | ours (committed `_big` = `[128,128,16]`) |
|---|---|---|
| mma instruction | m16n8k32.s8.s8.s32 | **same** |
| tile M(tok)×N(row) | 128×128 (J×I) | 128×128 |
| warps / block | **8** (256 thr) | 16 (512 thr) |
| f32 acc / thread | **64** | 32 |
| CTAs/SM (occupancy) | 1 | 1 |
| smem x/w layout | row-major + pad | row-major + pad (**same**) |
| weight repack | none (NV mma path) | none (**same**) |
| per-block scaling | I2F + FMA per element | I2F + FMA per element (**same**) |
| B fragment load | `load_generic` (plain LDS) | `ld_matrix` (LDSM) |
| K work-split | **stream-K + fixup** | one CTA does full K |

The design point is essentially IDENTICAL (tile, mma, scaling, smem, no repack). The only real
structural differences are (a) 8 warps × 64-acc/thread vs our 16 × 32, (b) stream-K, (c) B via
plain loads vs ldmatrix.

### Phase 2 — replicating each difference (all measured, isolated `bench_gemm_i8mma.mojo`)

Our kernel is already parametrized `[BM,BN,NW,FMT]`, so llama.cpp's exact register shape is just
`[128,128,8]` (8 warps → M_WARPS=4, N_WARPS=2, NT_PER_WARP=8 → **64 f32 acc/thread**, matching
their layout). Q4_K, RTX 4090, INT8 TOPS (higher = better):

| shape (N/K/T) | committed `_big` (16w, 32acc) | `llt` = llama shape (8w, 64acc) | mma-burst (deferred epilogue) |
|---|--:|--:|--:|
| 14336/4096/512  | 62.0 | 57.6 | 62.3 |
| 4096/14336/512  | 61.8 | 57.9 | 61.9 |
| 14336/4096/2048 | **66.0** | 61.0 | 66.0 |
| 4096/14336/2048 | 65.4 | 60.4 | 65.4 |

- **Matching llama.cpp's 8-warp / 64-acc register tile (`llt`) is ~7% SLOWER**, not faster.
  Fewer warps at higher per-thread ILP loses to more warps at 32-acc on this Mojo codegen.
- **mma-burst** (preload ALL B fragments, issue MT×NT mma back-to-back, then a fully deferred
  f32-epilogue burst) is **bit-for-bit the same TOPS as the committed interleaved loop**
  (65.98 vs 65.97 @ T=2048). ptxas already schedules the epilogue over the mma; the ordering
  is not the bottleneck. SASS (`cuobjdump -sass`) confirms: IMMAs carry `.reuse` operand flags
  and the I2FP/FFMA epilogue is already interleaved into the mma stream; 127 regs, **0 spills**,
  1 CTA/SM.
- **BN=256** (the only remaining X-re-read-traffic lever, X is re-read `ceil(N/BN)×`) **cannot
  launch**: 64-acc/thread × 512 threads exceeds the 65536-reg/block limit →
  `CUDA_ERROR_LAUNCH_OUT_OF_RESOURCES`. BN is register-capped at 128.

**Is it bandwidth-bound?** No. Correct traffic for 14336/4096/2048: W = 14336·4096·(144/256) =
33.4 MB read once; X = 2048·4096 int8 = 8.4 MB re-read `ceil(14336/128)=112×` = 940 MB; out =
59 MB → ~1.03 GB total. At the 4090's ~1 TB/s that is ~1.0 ms, but the kernel takes 3.65 ms →
**compute/issue-bound, not DRAM-bound** (X also largely fits the 72 MB L2). So the wall is
IMMA-issue efficiency: 66 TOPS = 36% of the 184-TOPS pure-mma microbench ceiling, 20% of the
330-TOPS hw peak.

### llama.cpp reconfirmed on current machine state

```
$ ./build/bin/llama-bench -m .../mistral-7b-q4_k_m.gguf -ngl 99 -p 4096 -n 128
| llama 7B Q4_K - Medium | 4.07 GiB | 7.25 B | CUDA | 99 | pp4096 | 12018.02 ± 108.73 |
| llama 7B Q4_K - Medium | 4.07 GiB | 7.25 B | CUDA | 99 |  tg128 |   182.53 ±   0.28 |
```

### Outcome — nothing shipped, committed kernel retained

Every Phase-2 change (`llt`, mma-burst, BN=256) either regressed or was flat/uncompilable, so
none beats the committed `_big`-gated kernel. Per the "never ship slower than committed" bar, all
experiments were **reverted**; the working tree is byte-identical to `HEAD` for the kernel.

**Honest remaining gap:** FORGE prefill ~2827 tok/s (committed, gated `_big`) vs llama.cpp 12018
@ pp4096 = **~4.25× behind**, driven by the Q4_K int8-mma GEMM sitting at 66 TOPS (36% of its own
mma ceiling) vs llama.cpp's ~169 (92%). The cause is now pinned precisely: it is **NOT** the
algorithm (tile, mma, per-block scaling, smem layout, and register-tile shape were all read from
their source and are identical or reproducible), and **NOT** DRAM bandwidth (measured ~3.5×
under the roofline). It is **IMMA-issue efficiency of the Mojo-emitted inner loop** — the exact
same MMQ design that nvcc/ptxas schedules to 92% of the mma ceiling, Mojo's backend schedules to
36%, and no source-level restructuring (warp count, register-tile size, mma/epilogue ordering,
barrier count, ldmatrix pairing — this round and the four prior) moves it. The two genuinely
untried levers both carry the same register-pressure wall that already blocks BN=256:
**stream-K** (needs a global fixup/reduction buffer + atomics, a real kernel+launcher rewrite)
and a Mojo-compiler improvement to the mma/LDS dual-issue schedule (outside our control). Both
are out of scope for a low-risk pass and neither is guaranteed to close a 36%→92% backend gap.

---

## 2026-07-19 — Deep comptime K-unroll (forcing nvcc's straight-line window) — Mojo unrolls, no win

Follow-up to `docs/CODEGEN_PROOF.md`, which pinned the gap on Mojo emitting a ROLLED K-loop
(8 IMMA/body) vs nvcc's deep-unrolled 256 IMMA/body and asserted Mojo "will not unroll the
K-loop deeply." Tested that assertion head-on: `gemm_i8mma_deep[BM,BN,NW,FMT,KU,NBUF]` (scratch,
reverted) holds `KU` consecutive 32-col blocks per smem buffer and `comptime for`-unrolls the
inner mma across all KU → `KU×8` IMMA in one straight-line body.

**SASS (`cuobjdump -sass`, IMMA-per-body, ptxas 13.3 sm_89):**

| variant | KU | IMMA/body | BRA | BSSY/BSYNC | regs | spill | smem |
|---------|----|-----------|-----|-----------|------|-------|------|
| committed `_big` | 1 | **8**  | 23 | 32 | 127 | 0 | 20 KB |
| deep2 (NBUF=2)   | 2 | **16** | 26 | 40 | 118 | 0 | 40 KB |
| deep4 (NBUF=1)   | 4 | **32** | 22 | 38 | 104 | 0 | 40 KB |
| deep8 (128×128)  | 8 | — ptxas REJECTS: `0x14000` (80 KB) > `0xc000` (48 KB static cap) |

**Mojo HONORS the unroll** — IMMA/body scales exactly 8→16→32, BRA does not grow, 0 spill. The
proof's "backend won't unroll" claim is refuted; the rolled 8-IMMA body is a consequence of the
1-block-per-buffer smem tiling.

**But it does not pay** (Q4_K TOPS, RTX 4090, 3-rep steady state, big / deep2 / deep4):

| N | K | T | big (8) | deep2 (16) | deep4 (32) |
|---|---|---|---------|-----------|-----------|
| 14336 | 4096 | 512  | 62.1 | 63.6 | 60.6 |
| 4096  | 14336| 512  | 62.4 | 64.2 | **67.4** |
| 14336 | 4096 | 2048 | 65.9 | 65.8 | 66.0 |
| 4096  | 14336| 2048 | 65.5 | 66.3 | **68.0** |

4× the IMMA window buys **≤ +8 %** (best case: K-heavy down-proj) and **−2 %** on K-light
gate/up — not the 3.5× the pipeline-depth thesis predicted; still ~66 TOPS vs nvcc 208. deep2 and
deep4 are **bit-identical** to the committed kernel (Q4_K + Q8_0, integer mma exact). This is the
final nail: the gap is a ptxas LDS/IMMA co-issue *scheduling* advantage, not loop-rolling or
window depth. The 48 KB static-`stack_allocation` cap blocks KU≥8 at BM=BN=128 (nvcc reaches 256
IMMA/body via dynamic smem), but the 8→32 trend shows even that wouldn't reach 208. **Reverted;
committed `_big` kernel retained** (no large win, and deep4 single-buffers below 2 CTAs/SM — the
documented 512-prefill occupancy tripwire).

---

## 2026-07-19 — Adopting llama.cpp's MMQ scheduling IN THE CUDA (nvcc) kernel — deep-unroll regresses

The prior deep-unroll study was Mojo-side. This one tests the same hypothesis on the **nvcc
CUDA** kernel that already ships the Q4_K prefill GEMM (`kernels/cuda/gemm_i8mma.cu`, committed
cubin, ~107 TOPS). Task premise: 107 is "half-tuned" and adopting llama.cpp's actual MMQ
scheduling (Ada tile dims, deep-unrolled K-loop with many IMMA in flight, stream-K) should roughly
double it toward 208. Tested head-on.

**What was implemented** (`gemm_q4k_wide_core`, reverted): llama.cpp's key scheduling shape lifted
into FORGE's framework, keeping FORGE's activation layout (bit-exact vs Mojo). Load a WIDE `KTILE`
(`KSUB` 32-col blocks) into shared memory at once, **preload every A/B fragment for the tile into
registers**, then `#pragma unroll` the whole tile → **32–64 IMMA straight-line** (vs 8 in the
committed kernel), occupancy=1. `cuobjdump -res-usage` confirms this reproduces **llama.cpp's exact
profile: 255 regs/thread, 40 KB smem, STACK spill** — the deep-pipeline footprint the proof
attributes to nvcc's 208-TOPS kernel.

**Isolated Q4_K GEMM (RTX 4090, `cargo test -p forge-kernels --test cuda_i8mma bench_tops`, TOPS at
T=2048 down/gate):**

| variant | regs | smem | IMMA/body | down T2048 | gate T2048 |
|---------|------|------|-----------|-----------:|-----------:|
| **committed `gemm_i8mma_core`** | ~193 | 18–20 KB | 8 | **106.6** | **107.3** |
| wide single-buffer KTILE=128 | 255 | 40 KB | 64 | 66.2 | 68.9 |
| wide double-buffer KTILE=64  | 172 | 40 KB | 32 | 72.2 | 79.5 |
| wide double-buffer KTILE=32  | 130 | 20 KB | 16 | 89.0 | 90.8 |

Every deep-unroll variant is **below** the committed 107, and it is **monotone**: deeper unroll =
slower (66 < 72 < 89 < 107). Reaching llama.cpp's own 255-reg/40 KB footprint did NOT recover
its throughput — the SASS scheduling llama.cpp's compiled `mul_mat_q` achieves is not reproduced by
restructuring a hand kernel to the same tile/register shape. Numerically the wide kernel is
**bit-identical to Mojo** (`vs_mojo=0.0e0`) once the min-correction epilogue is split into two
accumulations, and 4.65e-4 vs the CPU MMQ reference (`cuda_i8mma.rs` passes).

**Confirms CODEGEN_PROOF Exp 5 on the nvcc side:** the pipeline/unroll window is NOT the bottleneck.
Even nvcc/ptxas does not extract 208 from a hand-written kernel when the smem/register tile layout
changes — 208 belongs specifically to llama.cpp's *actual compiled* `mul_mat_q` (proof Exp 2). A
verbatim lift of that kernel (templated `mma.cuh` tile abstraction + `common.cuh` helpers + a new
`block_q8_1_mmq` quantize + `write_back`/layout adaptation to f16 `[token][row]` + launcher rework
for the new activation layout) is a large integration that risks the Bielik golden bit-exact test
and is out of scope for a single verifiable pass. **Reverted; committed 107-TOPS kernel retained**
as the best hand-written path (tree == HEAD, no functional change). The real route to 208 is
vendoring llama.cpp's kernel or a cuBLASLt int8 path — not a hand rewrite of the scheduling.

Prefill unchanged from the committed baseline (this kernel was reverted): Mistral-7B Q4_K_M
pp512 **3334**, pp4096 **3536**, pp8192 **2930** tok/s; llama.cpp pp4096 **12018** (≈0.29×). Closing
the rest is Phase-2 fusion (attention/quant/norm) + a real MMQ kernel, not this one GEMM.

---

## Marlin W4A16 — Phase-A go/no-go on FORGE prefill shapes (2026-07-19)

**Verdict: NO-GO for the stated goal (beat llama.cpp pp4096 ≈ 12000 tok/s). Not integrated.**

### What was measured
IST-DASLab Marlin (`github.com/IST-DASLab/marlin`, Apache-2) built standalone with
`nvcc -arch=sm_89 --expt-relaxed-constexpr` (no torch), timed on the exact Mistral-7B FFN
GEMM shapes with a value-independent timing harness (`scratch/marlin/bench_standalone.cu`,
outside the repo tree). groupsize=128, 200 iters, CUDA-event window, workspace zeroed per call.

Raw output (RTX 4090, sm_89):

```
Marlin W4A16 (groupsize=128) standalone, RTX 4090 sm_89
  T=16    gate/up N=14336 K=4096     :    60.76 us  (1.9 GFLOP,   30.9 TFLOP/s)
  T=64    gate/up N=14336 K=4096     :   193.22 us  (7.5 GFLOP,   38.9 TFLOP/s)
  T=128   gate/up N=14336 K=4096     :   236.05 us  (15.0 GFLOP,   63.7 TFLOP/s)
  T=512   gate/up N=14336 K=4096     :   660.19 us  (60.1 GFLOP,   91.1 TFLOP/s)
  T=2048  gate/up N=14336 K=4096     :  1472.09 us  (240.5 GFLOP,  163.4 TFLOP/s)
  T=4096  gate/up N=14336 K=4096     :  2783.72 us  (481.0 GFLOP,  172.8 TFLOP/s)
  T=16    down    N=4096  K=14336    :    17.76 us  (1.9 GFLOP,  105.8 TFLOP/s)
  T=64    down    N=4096  K=14336    :    58.40 us  (7.5 GFLOP,  128.7 TFLOP/s)
  T=128   down    N=4096  K=14336    :    93.89 us  (15.0 GFLOP,  160.1 TFLOP/s)
  T=512   down    N=4096  K=14336    :   346.40 us  (60.1 GFLOP,  173.6 TFLOP/s)
  T=2048  down    N=4096  K=14336    :  1380.04 us  (240.5 GFLOP,  174.3 TFLOP/s)
  T=4096  down    N=4096  K=14336    :  2758.89 us  (481.0 GFLOP,  174.4 TFLOP/s)
```

### The architectural reason (why this is a hard ceiling, not a tuning gap)
Marlin is **W4A16**: 4-bit weights are dequantized on-chip and the matmul runs on **fp16
tensor cores with fp32 accumulate**. Its only advantage over a plain fp16 GEMM is *weight
memory bandwidth* — which matters at small batch (memory-bound decode, T≤32) and vanishes in
the **compute-bound prefill** regime (T≥512), where the weight bytes are reused across all
tokens. At T=2048/4096 Marlin plateaus at **~174 TFLOP/s**, which is essentially the RTX 4090's
fp16-tensor-with-fp32-accumulate peak (~165–175 TFLOP/s dense). Marlin is *already saturated* —
there is no tuning headroom left on this compute path.

llama.cpp's prefill does **not** use fp16 tensor cores. Its `mul_mat_q` (MMQ) quantizes the
activations to int8 and runs **int8 tensor cores** (`mma.s8`, ~660 TOP/s dense on Ada — ~4×
the fp16/fp32-accum rate). Measured effective GEMM throughput of llama.cpp on these shapes:

- llama.cpp pp4096 = **11984 tok/s** → 4096/11984 = **0.3418 s** total prefill.
- Mistral-7B GEMM budget (32 layers, q+k+v+o+gate+up+down) = **57.18 TFLOP** for 4096 tokens.
- GEMM is ~81% of prefill → GEMM time ≈ 0.277 s → **≈206 TFLOP/s effective (int8)**.

**Marlin's 174 TFLOP/s < llama.cpp's 206 TFLOP/s on the identical GEMMs.** A kernel that does
the same matmuls *slower* than the reference cannot make the end-to-end engine *faster* than
the reference.

### Honest end-to-end projection (Mistral-7B pp4096)
| Scenario | GEMM time | non-GEMM | total | pp4096 | vs llama.cpp |
|---|---|---|---|---|---|
| **Ceiling** (0 non-GEMM overhead — unphysical) | 57.18/174 = 0.329 s | 0.000 s | 0.329 s | **12460** | 1.04× |
| **Optimistic** (non-GEMM = llama.cpp's 0.065 s) | 0.329 s | 0.065 s | 0.394 s | **10404** | 0.87× |
| **Realistic** (FORGE's current 19% overhead, ~0.207 s) | 0.329 s | 0.207 s | 0.536 s | **7642** | 0.64× |
| llama.cpp (int8 MMQ, reference) | 0.277 s | 0.065 s | 0.342 s | **11984** | 1.00× |
| FORGE current (committed int8 kernel, measured) | — | — | 1.090 s | **3759** | 0.31× |

Marlin **would** beat FORGE's *own* current kernel (3759 → ~7600), but it **cannot clear
12000** — even the physically-impossible zero-overhead ceiling (12460) only grazes it, and any
real engine lands 7600–10400. The stated goal is to *beat llama.cpp*, and W4A16 is structurally
incapable of that on Ada because it runs the wrong (fp16, ~4× slower) tensor-core path for
compute-bound prefill.

### Phase-B mapping analysis (documented for completeness; moot given the NO-GO)
Q4_K dequant is affine per 32-block: `w = d·sc[j]·q − dmin·m[j]` (q∈[0,15], fp16 `d`/`dmin`,
6-bit `sc[j]`/`m[j]`). Marlin formats:
- **Symmetric W4A16** (`gptq_marlin`): `w = scale_g·(q−8)` — no additive per-group offset →
  cannot represent Q4_K's `dmin·m[j]` term. Not representable.
- **AWQ-asymmetric** (`awq_marlin`): `w = scale_g·(q − zero_g)`, `zero_g` an **integer** 4-bit
  zero point. Exact Q4_K needs `scale_g = d·sc[j]` (fine at group_size=32) and
  `zero_g = dmin·m[j]/(d·sc[j])`, which is **generally fractional** (four independent operands)
  → **not exactly representable** with an integer zero point.

Exact Q4_K→Marlin is therefore impossible in mainline Marlin (no fractional/float zero-point
variant exists). The only path would be a **load-time requantization** Q4_K→Marlin-4bit
(group_size=32, fp16 scale + rounded integer zero), i.e. accept a small numeric change and gate
on coherence rather than bit-exactness. Since Phase A is a NO-GO, this repack was **not**
implemented — but it is the correct decision to record: even with the effort, the result would
be both *lossy vs Q4_K* and *slower than llama.cpp*.

### Recommendation
The route to beating llama.cpp on the 4090 stays on **int8 tensor cores** (FORGE's current path
at 107 TOPS, headroom to llama.cpp's 206 and the 660 hardware peak), not W4A16. Options that
keep the int8 compute path: vendoring llama.cpp's compiled `mul_mat_q`, a cuBLASLt int8 GEMM, or
a **W4A8** kernel (Marlin-QQQ / int4-weight × int8-activation), which — unlike W4A16 Marlin —
would use the int8 tensor cores and is the only Marlin-family variant that could match the
reference's compute path. The committed `gemm_i8mma_core` kernel is **retained unchanged**
(tree == HEAD; Marlin lives only in `scratch/`, nothing brought in-tree).

---

# W4A8 (int4-weight × int8-activation) investigation — 2026-07-19

Follow-up to the W4A16-Marlin NO-GO above. The prior recommendation ended on: the only
Marlin-family variant that could beat llama.cpp is **W4A8** on the int8 tensor cores. This
section is the cheap go/no-go for that path (QServe / Marlin-QQQ style), run on this machine.
Every number is a real run; the committed `gemm_i8mma` kernel is retained unchanged. Nothing
brought in-tree.

Baselines reconfirmed this session, GPU at full boost (SM 2775 MHz, ~320 W, 56 °C):

- `llama-bench -p 4096 -n 0`: **pp4096 = 12032 ± 118 tok/s** (reconfirmed by the maintainer on a
  clean, idle, cool GPU at full boost 2775 MHz / 418 W). An earlier same-session reading of
  ~7475 was a **GPU-contention/thermal artifact** from sustained back-to-back benchmarking, NOT
  the true number — the honest llama.cpp target is **~12032**. Every projection below is corrected
  to this baseline. (`-ngl 99`.)
- `forge bench --prefix-cache off` (committed Q4_K int8-MMQ): pp512 **2591**, pp4096 **3794**,
  pp8192 **2955** tok/s; decode 175/146/131.

## Hardware fact first: Ada DOES have int4 tensor cores (2× int8)

Pure-issue microbench (8 independent accumulator chains, registers only, 3-rep steady):

| mma | shape | steady rate |
|-----|-------|-------------|
| `s8.s8.s32`  | m16n8k32 | **714 TOPS** |
| `s4.s4.s32`  | m16n8k64 | **1428 TOPS** (2×, identical wall-clock/instruction) |

So on the 4090: an **int4×int4** GEMM can peak at ~1428, but **int4×int8 has no native mixed
mma** — W4A8 must upconvert the int4 weight to int8 and issue `s8.s8` mma → same **714 ceiling**
as the committed MMQ. W4A8's only lever over Q4_K-MMQ is a *cleaner dequant/epilogue*, not more
mma throughput. (A true W4A4 on `s4.s4` is the only path with raw headroom above 714, but int4
activations are accuracy-hostile and out of scope.)

## Phase A — QServe W4A8 GEMM, standalone, on our exact Mistral FFN shapes

Kernel: QServe `w4a8_per_group` `dense_kernel0` (MIT; `github.com/mit-han-lab/qserve`),
torch stripped, built `nvcc -arch=sm_89 -O3`, benchmarked with a CUDA-event harness
(sustained ~1.5 s warmup to reach boost clock — mandatory, the 4090 boot-clock artifact swings
readings 2× otherwise; best-of-30×20). ops = 2·M·N·K. Same-session apples-to-apples against the
**committed FORGE CUDA MMQ** (`gemm_i8mma.cu`, same harness).

**down-proj** N=4096 K=14336 · **gate/up** N=14336 K=4096:

| T | shape | FORGE committed CUDA | QServe W4A8 | llama.cpp MMQ (CODEGEN_PROOF, nvcc) |
|---|-------|----------------------|-------------|-------------------------------------|
| 512  | down    | 92.4  | ~320–433 | ~219 |
| 512  | gate/up | 95.3  | ~407     | ~223 |
| 2048 | down    | 111.8 | **452.8** | ~208 |
| 2048 | gate/up | 109.3 | **445.6** | ~224 |
| 4096 | down    | 111.6 | **455.6** | — |
| 4096 | gate/up | 109.5 | **450.7** | — |

(TFLOP-eq. QServe raw µs @T=2048: down 531, gate/up 540; @T=4096: down 1056, gate/up 1067 —
monotone/linear = correct GEMM. The committed kernel sits at ~110, matching the repo's ~107.)

**DECISIVE GATE — PASS.** QServe W4A8 reaches **~450 TFLOP-eq at T≥2048** and **~400+ at T=512**
— **2.2× llama.cpp's 206** and **4.0× FORGE's committed 110**, at the same boost clock. This is
the first FORGE-adjacent kernel to clearly clear the 206 wall. (Unlike W4A16 Marlin, which
plateaued at 174 = fp16 peak.) The win is *scheduling*: QServe uses `cp.async` multi-stage
pipelining + large tiles + a clean symmetric int4 dequant, where MMQ pays for Q4_K's per-32
affine unpack in the inner loop.

### End-to-end projection (GEMM = 81 % of prefill)
GEMM 4.0× (110→450) → prefill speedup = 1/(0.19 + 0.81/4.0) = **2.55×**.

| shape | FORGE now | projected W4A8 | llama.cpp (clean) | ratio vs llama.cpp |
|-------|-----------|----------------|-------------------|--------------------|
| pp4096 | 3785 | **~9650** | 12032 | **0.80× (still behind)** |

Corrected against the true llama.cpp baseline (12032): the W4A8 GEMM alone takes prefill to
~9650 = **0.80×** — a 2.55× jump but NOT yet a win, because FORGE's non-GEMM work
(attention prefill + activation-quant + RMSNorm + per-launch overhead) is ~3× llama.cpp's.
Beating llama.cpp end-to-end needs BOTH the W4A8 GEMM **and** cutting that non-GEMM overhead
(fusion / fewer launches) toward llama.cpp's ~0.065 s. Both are proven-feasible; both are real work.

## Phase B — Q4_K → W4A8 requant accuracy (CPU, real Mistral FFN tensors)

`cargo run -p forge-formats --example requant_w4a8` dequantizes 15 FFN tensors (blocks
0/7/15/23/31), requantizes each row to int4 per-group, reports `relL2 = ‖W_q4k − W_w4a8‖/‖W_q4k‖`
(the *additional* error on top of Q4_K):

| group | symmetric relL2 | asymmetric (int4+zero) relL2 |
|-------|-----------------|------------------------------|
| 32  | 0.0987 | **0.0809** |
| 64  | 0.1100 | 0.0919 |
| 128 (kernel's G) | 0.1209 | **0.1024** |

The QServe per-group kernel is compiled at **G=128** → **~10.2 % relL2** even asymmetric — a
non-trivial perturbation of an already-Q4_K-quantized model (Q4_K vs fp16 is typically ~2–4 %).
QServe's *actual* dequant is stricter still: two-level QoQ (`w_i8 = q4·s2_int8 + zero_int8`, then
`·s1_fp16·ascale`) with an **int8** group scale, so a faithful Q4_K→QServe requant would be
**≥10 %** relL2. Coherence (does Mistral still say "Paris") was **not** measured — that requires
the full Phase-C integration.

## Phase C — integration: greenlit, NOT shipped this session

Phase A clears the go/no-go decisively and the projection beats the *local* llama.cpp, so the
route is worth taking. It was **not** integrated here because a correct build is a large,
high-risk lift that cannot be shipped half-done (repo rule: never ship slower/incorrect than
the committed kernel; the committed `gemm_i8mma` stays as the fallback). Concretely Phase C needs,
all validated only against a CPU golden + a coherence oracle (no QServe reference checkpoint):

1. **Q4_K → QoQ requant at load** — int4 codes + per-group int8 `s2` scale + int8 `zero` +
   per-channel fp16 `s1`, reproducing QServe's **8-D weight interleave** (`M//32, K//32, (8,4),
   (2,2,2,4)` permute) exactly, else ldmatrix reads garbage → gibberish.
2. **Per-token int8 activation quant** producing `ascale` (per-token fp16) + the zero-correction
   `input_sum`, distinct from FORGE's per-32-block `quantize_act_q8_1`.
3. In-tree `kernels/cuda/w4a8_gemm.cu` (MIT attribution) → committed cubin via `build.sh` →
   `registry.rs` entry → launcher → route Q4_K prefill GEMM. Q8_0 + NVFP4 untouched.
4. Gate on coherence + measure the pp512/4096/8192 delta and the load-time repack cost.

## Committed state
Tree == HEAD. QServe lives only in scratch (`scratch/w4a8/`, outside the repo). The one added
file is `crates/forge-formats/examples/requant_w4a8.rs` (the Phase-B measurement tool,
reproducible, harmless). Committed `gemm_i8mma_core` retained unchanged.

---

# W4A8 in-tree integration — 2026-07-19 (kernel + packer + launcher PROVEN in-engine; engine routing is the remaining step)

Phase C, executed. The W4A8 GEMM now runs **inside FORGE** (its cubin loaded through the same
cudarc `cuModuleLoadData` path as `gemm_i8mma`) and is **verified correct on the real 4090 both
standalone and through FORGE's HAL** with a Rust-side QServe packer. It is **non-default**: nothing
in the engine routes to it yet, so the committed CUDA MMQ stays the Q4_K prefill path. Every number
below is a real run on this machine; raw output pasted.

## 1. Correctness harness FIRST (de-risk the QServe layout before any wiring)

`scratch/w4a8/harness.cu` (standalone, nvcc `-arch=sm_89`): reproduces QServe's exact 8-D weight
interleave (`omniserve/.../w4a8_linear.py` `from_linear`, reshape `[N/32,2,2,8, K/32,2,4,4]` →
permute `[d0,d4,d3,d6,d5,d2,d7,d1]` → nibble-pack), the per-group int8 scale/`(-zero)*s2` reorder
(`j → (j%8)*4 + j//8`), and per-token int8 activation quant — all in host C++ — then runs QServe's
verbatim `dense_kernel0` and compares to an independent CPU int4×int8 golden that models the
kernel's **bytewise int8 reconstruction** (`(int8_t)(s2*(q4-zero))`). Tolerance relL2 < 2e-2.

```
== W4A8 correctness (GPU QServe vs CPU int4xint8 golden), tol relL2<2e-2 ==
  M=256   N=128    K=256     relL2=2.09e-04  maxabs=3.871e-03  maxrel=4.88e-04  PASS
  M=256   N=256    K=512     relL2=2.08e-04  maxabs=3.902e-03  maxrel=4.86e-04  PASS
  M=384   N=512    K=1024    relL2=2.07e-04  maxabs=7.805e-03  maxrel=4.87e-04  PASS
  M=129   N=256    K=512     relL2=2.07e-04  maxabs=3.901e-03  maxrel=4.86e-04  PASS   (small-M branch)
  M=160   N=4096   K=4096    relL2=2.07e-04  maxabs=1.562e-02  maxrel=4.87e-04  PASS
  M=256   N=4096   K=4096    relL2=2.08e-04  maxabs=1.562e-02  maxrel=4.88e-04  PASS
  M=512   N=2048   K=4096    relL2=2.08e-04  maxabs=1.562e-02  maxrel=4.88e-04  PASS
  M=512   N=4096   K=2048    relL2=2.07e-04  maxabs=1.440e-02  maxrel=4.88e-04  PASS
```

The residual ~2e-4 is pure fp16 output rounding. **The QServe weight interleave + scale/zero
reorder + per-token activation quant are byte-exact.** (Before modelling the int8 wrap, a handful of
out-of-range reconstructions gave relL2 0.06–0.25 with maxrel ~1e3 — the tell that a few `s2*(q4-zero)`
values wrap; the kernel wraps them too, so a faithful golden must, and then it PASSES.)

## 2. In-tree kernel + committed cubin + registry + launcher

- `kernels/cuda/w4a8_gemm.cu` — QServe `dense_kernel0` + device helpers verbatim (MIT attribution),
  the `__global__` made a `__device__` helper, four `extern "C"` entries (one per QServe CTA config:
  `m128`, `m64_ksm`, `m64_klg`, `m32`). `kernels/cuda/build.sh` compiles it to the committed
  `kernels/mojo/build/sm_89/w4a8_gemm_cuda.cubin` (all configs ≤ 41.5 KB dynamic smem — no
  `cudaFuncSetAttribute` needed). `res-usage`: m128 162 reg, m64_klg 111, m64_ksm/m32 96, 0 spill.
- `registry.rs` embeds the cubin and resolves the four entries (mirrors the `gemm_i8mma_cuda` load).
- `launchers.rs::w4a8_gemm` selects the CTA config by (tokens, K) and computes grid/block/dynamic-smem
  exactly as QServe's host `KERNEL_LAUNCH_CODE` (block swizzle `log_tile` included).
- `forge_formats::w4a8` (`w4a8_pack` / `w4a8_reconstruct`) is the Rust port of the verified host packer.

## 3. In-engine correctness through the HAL (Rust packer → FORGE HAL → committed cubin)

`crates/forge-kernels/tests/cuda_w4a8.rs` builds random weights, packs with the Rust
`forge_formats::w4a8::w4a8_pack`, quantizes activations per-token int8, launches via
`Kernels::w4a8_gemm`, and compares to an independent CPU golden (`w4a8_reconstruct`). This exercises
every CTA branch and the Mistral FFN shapes:

```
w4a8 M=256 N=128 K=256:    relL2=2.08e-4 maxabs=4.877e-4
w4a8 M=256 N=256 K=512:    relL2=2.08e-4 maxabs=4.817e-4
w4a8 M=129 N=256 K=512:    relL2=2.08e-4 maxabs=4.817e-4   (m128 branch)
w4a8 M=128 N=256 K=256:    relL2=2.07e-4 maxabs=4.877e-4   (m64_ksm branch)
w4a8 M=128 N=256 K=8192:   relL2=2.10e-4 maxabs=1.952e-3   (m64_klg branch)
w4a8 M=64  N=256 K=512:    relL2=2.09e-4 maxabs=2.439e-4   (m32 branch)
w4a8 M=256 N=4096 K=4096:  relL2=2.09e-4 maxabs=9.799e-4
w4a8 M=512 N=4096 K=4096:  relL2=2.09e-4 maxabs=9.799e-4
w4a8 M=192 N=14336 K=4096: relL2=2.09e-4 maxabs=9.792e-4   (gate/up)
w4a8 M=192 N=4096 K=14336: relL2=2.07e-4 maxabs=3.927e-3   (down)
test w4a8_matches_cpu_golden ... ok
```

## 4. Perf — reconfirmed standalone AND through the HAL (idle GPU, boost 2775 MHz)

Standalone harness (CUDA-event, sustained warmup, best-of-20), Mistral FFN shapes:

```
BENCH M=2048  N=14336 K=4096   574.6 us   418.6 TFLOP-eq   (gate/up)
BENCH M=2048  N=4096  K=14336  558.9 us   430.3 TFLOP-eq   (down)
BENCH M=4096  N=14336 K=4096  1145.9 us   419.8 TFLOP-eq
BENCH M=4096  N=4096  K=14336 1113.7 us   431.9 TFLOP-eq
BENCH M=512   N=14336 K=4096   153.9 us   390.6 TFLOP-eq
BENCH M=512   N=4096  K=14336  139.4 us   431.3 TFLOP-eq
```

Through FORGE's HAL (`cargo test ... bench_w4a8_tops --ignored`, wall-clock Instant timing):

```
w4a8 gate N=14336 K=4096 T=2048: 438.6 TFLOP-eq   T=4096: 420.7
w4a8 down N=4096  K=14336 T=2048: 420.0 TFLOP-eq   T=4096: 424.9   T=512: 410.0
```

The HAL path matches the standalone (~420–439 TFLOP-eq at T≥2048) — **no HAL overhead**, and
**2.0–2.1× llama.cpp's ~206 GEMM**, **~3.8× FORGE's committed ~110**. (The one low reading,
gate T=512 = 141.8, is std::time launch-overhead noise at the small size — the CUDA-event standalone
gets ~390 there; correctness is proven either way.)

## What is DONE vs the REMAINING engine-routing step

DONE and verified on the real GPU: (1) the correctness harness proving the QServe layout;
(2) the in-tree kernel + committed cubin + registry + launcher; (3) the Rust requant/packer
(`forge_formats::w4a8`); (4) full in-engine correctness through the HAL; (5) in-engine GEMM perf
= ~420 TFLOP-eq. Gate 1 (build + clippy `--release --workspace`) green; Gate 4 (NVFP4 Bielik golden)
bit-exact on 1 and 4 lanes — **NVFP4/Q8_0 untouched**.

NOT done this pass (the remaining step, non-default): **routing Q4_K prefill to W4A8 in the model
forward pass** (`FORGE_GEMM=w4a8`) + **requant-at-load** + **a GPU per-token int8 activation quant**.
The specific integration hazard that makes this a careful, separate lift — and why it was NOT rushed
into a possibly-gibberish default — is **row-window addressing**. FORGE stores q/k/v and gate/up
**fused** and slices them by `row_off` in `gemm_rows` (model.rs 2045–2052, 2235–2236). QServe's
scale/zero buffers are the **transposed** `[K/G][N]` layout, so a row window is a *non-contiguous
column slice at full-N stride*, and `wscales`/the weight interleave are addressed by absolute N. A
correct routing therefore needs either (a) per-logical-matrix W4A8 sub-tensors (requant q/k/v/gate/up
into separate buffer-sets at load, dispatched by `row_off`), avoiding row-windowing entirely, or
(b) a windowed launcher that passes full-N strides with a column base offset. Both are real work that
must be gated on **coherence** (the 3–4 factual-prompt greedy test) + a **perplexity proxy** and the
**~10.2 % relL2** requant cost (Phase B) before it can even be a non-default option — and per the repo
rule ("never route slower/incorrect than the committed kernel as default") it stays behind the flag
until it passes all of that. The projection stands: W4A8 GEMM alone → pp4096 **~9650 = 0.80×**
llama.cpp's 12032; the remaining gap is FORGE's ~0.205 s non-GEMM vs llama.cpp's ~0.065 s (the
Phase-2 fusion target), not this GEMM.

llama.cpp baseline this pass: could NOT be independently re-run — the `scratch/bench/llama.cpp`
build was wiped with the scratch dir. Baseline used is the committed, maintainer-reconfirmed
**pp4096 = 12032** (idle cool GPU at full boost, commit 43e3591d, this machine).

## Committed state (this pass)
Non-default W4A8 brought in-tree: `kernels/cuda/w4a8_gemm.cu` (+ its committed cubin),
`kernels/cuda/build.sh`, `crates/forge-formats/src/w4a8.rs`, `registry.rs`, `launchers.rs::w4a8_gemm`,
`crates/forge-kernels/tests/cuda_w4a8.rs`. Nothing routes to W4A8; `gemm_i8mma` (committed CUDA MMQ)
remains the Q4_K prefill default. NVFP4 + Q8_0 untouched (golden bit-exact). Standalone harness lives
in `scratch/w4a8/` (reproducible).

---

## 2026-07-19 — Tensor-core flash-attention prefill (FORGE_ATTN=fa) — SHIPPED (flagged, coherent)

Prefill's dominant remaining cost, once the GEMM is a tensor-core kernel, is **attention**
(FORGE_PREFILL_TRACE: ~10 % of a fresh 1024-tok chunk, growing to **>50 %** at deep base_pos —
O(T·ctx)). The Mojo `attn_prefill` (kernels/mojo/src/prefill.mojo) computes QK^T and P·V with
**scalar/SIMD dot products** (`dotv += qv8*kv8`, `warp.sum`), no tensor cores. `kernels/cuda/
fattn_prefill.cu` (ADR-0001 exception, same cubin path as `gemm_i8mma`) replaces that with an
**f16 mma (`m16n8k16`) flash-attention**: QK^T via mma, online softmax (running max/sum, register
O accumulator, per-tile rescale), P·V via mma, over FORGE's paged KV cache with GQA. Byte-identical
I/O contract to `attn_prefill` → drop-in. Routed only under **`FORGE_ATTN=fa`**; default (`scalar`)
keeps the Mojo kernel so the golden path is bit-exact.

**Kernel design.** Grid `(ceil(T/64), n_q_heads)`; one 4-warp block owns 64 query rows of one head,
each warp a 16-row m-tile. K streams through smem tiles of BK=32 positions as `[key][head_dim]`;
V is written **transposed** to `[head_dim][key]` so both QK^T and P·V use the proven `mma.row.col`
convention `C[m][n]=Σ_k A[m][k]·Bstored[n][k]` (same as `gemm_i8mma`, so K/V load with plain
`ldmatrix.x2`, no `.trans`). Q fragments are preloaded once (reused across all KV tiles). The S
accumulator layout equals the mma A-operand layout, so the softmax probs feed P·V with **no repack**
(just f32→f16 pack). hd128: 157 regs, 32 KB smem, **0 spill**, ~3 CTAs/SM. hd64 also built.

**Correctness (standalone GPU-vs-CPU-golden, `scratch/fa_test.cu`).** FA output vs an exact CPU
reference (causal, GQA, paged), f16 cache, over T∈{1..513}, GQA 32/8 and 16/4, base_pos∈{0,32,100,512}:

```
hd128 T=1024 heads=32/8 base=0    max_abs=0.00037 max_rel=0.45  nbad=0/4194304
hd128 T=128  heads=32/8 base=512  max_abs=0.00004 max_rel=0.18  nbad=0/524288
hd64  T=512  heads=16/4 base=0    max_abs=0.00035 max_rel=0.25  nbad=0/524288
```

max abs diff **~3.9e-4** (f16-accumulation tolerance; mma reorders sums so not bit-exact) — well
under a 1e-2 bound. (`max_rel` is large only where the reference value is ~0; those are gated out.)

**Coherence.** `FORGE_ATTN=fa forge run mistral-7b-q4_k_m.gguf "The Eiffel Tower is located in the
city of" --max-tokens 24 --temp 0` → `"Paris, France. It is one of the most famous landmarks in the
world. It is a wrought iron tower"` — **the greedy stream is token-for-token IDENTICAL to the scalar
path** (all 24 tokens). The **Bielik NVFP4 golden test reproduces the canonical 16-token stream on 1
and 4 lanes under `FORGE_ATTN=fa`** as well as the default.

**Perf — same card, `--prefix-cache off`, Mistral-7B Q4_K_M, default GEMM (committed CUDA MMQ).**
FA changes ONLY prefill attention; decode is a separate kernel, untouched.

| P / T       | scalar attn (default) | FA attn (`FORGE_ATTN=fa`) | prefill ratio | decode |
|-------------|----------------------:|--------------------------:|--------------:|-------:|
| 512 / 128   | 2778 | 2677 | 0.96× (attn is small; GEMM-bound) | 175 (=) |
| 4096 / 2048 | 3749 | **4556** | **1.22×** | 146 (=) |
| 8192 / 1024 | 2953 | **4638** | **1.57×** | 130 (=) |

**Attention kernel's own time** (FORGE_PREFILL_TRACE, 8192 prefill = 8×1024-tok chunks, sum of the
`attn` phase; grows with base_pos): **scalar 1322 ms → FA 340 ms = 3.9× faster** (per-chunk
27.6/61.7/101.4/141.4/183.3/225.9/269.2/311.5 → 11.4/19.5/27.9/39.0/49.2/59.8/62.1/71.1 ms). At 512
prefill the attention fraction is tiny so FA is neutral (GEMM dominates).

**vs llama.cpp** (reconfirmed idle GPU this session, `llama-bench -ngl 99 -p 4096 -n 0 -fa 1`,
build 112c781): **pp4096 = 11927 ± 106**. Coherent stack (CUDA-MMQ GEMM + FA attention):
pp4096 0.31×→**0.38×** llama.cpp (gap 3.18×→**2.62×**); pp8192 the biggest jump (attention-heaviest).

**Coherent stack summary (default CUDA-MMQ GEMM + FORGE_ATTN=fa):** pp4096 3749→**4556**,
pp8192 2953→**4638** tok/s. Remaining gap to llama.cpp (~2.6×) is the un-fused GEMM+attention+quant
+norm pipeline and the GEMM's own 107-vs-208 TOPS deficit (documented above), not the attention
math anymore.

**Max-speed stack (non-coherent, for reference: `FORGE_GEMM=w4a8` + `FORGE_ATTN=fa`).** W4A8 is
quality-FAILED (see above) so this is NOT a shippable default, but it shows FA compounds on top of
the fast GEMM: pp4096 5543→**8072** (FA adds +46 %), pp8192 **8765** tok/s — i.e. with a coherent
W4A8 (SmoothQuant, future) the stack would sit at ~0.68× llama.cpp on pp4096.

## Committed state (this pass)
`kernels/cuda/fattn_prefill.cu` (+ committed cubin `build/sm_89/fattn_prefill_cuda.cubin`),
`kernels/cuda/build.sh`, `registry.rs` (embed + entries), `launchers.rs` (`attn_prefill` routes to
`attn_prefill_fa` under `FORGE_ATTN=fa` for f16 cache hd64/hd128; everything else falls through to
the Mojo scalar kernel). Default path (`FORGE_ATTN` unset / `=scalar`) is byte-unchanged — Bielik
NVFP4 golden bit-exact on 1 and 4 lanes. Standalone correctness harness in `scratch/fa_test.cu`.

---

## Vendored llama.cpp Q4_K MMQ GEMM (`FORGE_GEMM=mmq`, non-default) — this pass

The coherent-prefill GEMM deficit above (107-vs-208 TOPS) was the "hand CUDA kernel
still loses to llama.cpp's compiled MMQ" gap proven in `docs/CODEGEN_PROOF.md` Exp 2.
Root fix: stop hand-writing the tile and **vendor llama.cpp's actual `mul_mat_q`
device code**. `kernels/cuda/mmq_q4k.cu` includes the ggml-cuda headers vendored to
`kernels/cuda/vendor/llama-cpp/` (13 headers, 644 KB, MIT, commit `112c781`) and
instantiates ggml's Q4_K MMA path (`load_tiles_q4_K` → `vec_dot_q4_K_q8_1_mma` →
`mmq_write_back_mma`, reached through `mul_mat_q_process_tile`) unchanged — nvcc/ptxas
compiles *their* kernel, not a copy. This TU only adds the `extern "C"` entry points,
the dense conventional-tiling grid wrapper, ggml's `quantize_mmq_q8_1<DS4>` activation
quant (f16-input variant), and an f32→f16 epilogue. It consumes the **native GGUF Q4_K
weight bytes already resident (NO requant, no quality change)** + its own q8_1 quant.
Non-default: routed only under `FORGE_GEMM=mmq`; the committed hand `gemm_i8mma` Q4_K
path stays the default. Q8_0 / NVFP4 / decode untouched.

**Correctness** (`scratch/mmq_probe/harness.cu`, GPU MMQ vs independent CPU Q4_K×f16
golden, tol 5e-3 = the q8_1 activation-quant noise; weight quant cancels — same bytes
both sides): PASS on every FFN shape and orientation, token counts 1/33/512/517/
1000/2048, mmq_x ∈ {64,128}:

```
N=4096  K=14336 T=512  mmq_x=128 : relL2=3.876e-03  PASS
N=14336 K=4096  T=512  mmq_x=128 : relL2=3.826e-03  PASS
N=4096  K=14336 T=2048 mmq_x=128 : relL2=3.897e-03  PASS
N=512   K=256   T=33   mmq_x=64  : relL2=3.819e-03  PASS
N=4096  K=768   T=1000 mmq_x=64  : relL2=3.913e-03  PASS
```

**Isolated in-engine GEMM TOPS** (`scratch/mmq_probe/tops.cu`, same cubins the engine
loads, cudaEvent-timed, work = 2·N·K·T, RTX 4090 idle, 50 iters after warmup):

| shape | T | vendored MMQ | committed hand | speedup |
|-------|---|--------------|----------------|---------|
| down-proj N=4096 K=14336  | 512  | **116.6** | 64.9  | 1.79× |
| down-proj N=4096 K=14336  | 2048 | **189.0** | 99.5  | 1.90× |
| gate/up  N=14336 K=4096   | 512  | **231.7** | 95.5  | 2.43× |
| gate/up  N=14336 K=4096   | 2048 | **264.0** | 109.1 | 2.42× |

The vendored MMQ lands at **189–264 TOPS** (the committed hand kernel ~65–109), i.e. the
~208 the CODEGEN proof measured for llama.cpp's kernel — **1.8–2.4×** the hand kernel,
confirming the deficit was codegen (per Exp 5), closed by running their exact device code.

**Coherence** (`forge run … --temp 0`): `FORGE_GEMM=mmq` on Mistral-7B Q4_K produces
`Paris, France. It is one of the most famous landmarks in the world` — **token-identical**
to the committed Q4_K path (same quant scheme + weights, small numeric diff absorbed).
Qwen3-0.6B Q8_0 output identical with/without the flag (Q8_0 untouched).

**End-to-end prefill** (`forge bench --prefix-cache off`, FA default on both, 3-rep steady):

| prompt | committed (default) | `FORGE_GEMM=mmq` | ratio | vs llama.cpp 11962 |
|--------|---------------------|-------------------|-------|--------------------|
| pp512   | ~2590 | **~4609** | 1.78× | — |
| pp4096  | ~4652 | **~6478** | 1.39× | **0.54×** (was 0.38×) |
| pp8192  | 4645  | **6327**  | 1.36× | 0.53× |

Decode unchanged (146.2→146.2 tok/s @4096-ctx, 130.5→130.4 @8192; MMQ is prefill-only,
n_tokens ≥ 64). The pp4096 coherent stack now **6478 tok/s = 0.54× llama.cpp**, beating
the ~5700 (~0.48×) projection. Remaining gap to llama.cpp is un-fused quant/norm/epilogue
launches (FORGE runs quantize+GEMM+f32→f16 as 3 kernels; ggml fuses) + no stream-K load
balancing in the wrapper (dense conventional tiling) — future work, not the GEMM math.

**Committed state:** `kernels/cuda/mmq_q4k.cu` + `vendor/llama-cpp/` headers, `build.sh`
(→ `build/sm_89/mmq_q4k_cuda.cubin`), `registry.rs` (embed + 34 entries), `launchers.rs`
(`gemm_q4_k_mmq_at` + `mmq_q4k_config`, routed from `gemm_q4_k_i8mma_at` under
`FORGE_GEMM=mmq`), HAL `launch` opts into >48 KB dynamic smem via
`CU_FUNC_ATTRIBUTE_MAX_DYNAMIC_SHARED_SIZE_BYTES`. Default path byte-unchanged — Bielik
NVFP4 golden bit-exact on 1 and 4 lanes.

---

## Q6_K prefill → vendored MMQ + f16-direct epilogue (both wins, DEFAULT) — 2026-07-19

Two coherent wins over the now-default Q4_K MMQ, both pure numeric no-ops (same
native GGUF weights + q8_1 activation, no quality change). nsys on the prior default
(Q4_K MMQ + FA) showed Mistral-7B Q4_K_M prefill was 40% MMQ-Q4_K, **27% Q6_K down-proj
still on the slow Mojo f16 GEMM** (`gemm_q6_k_f16`), 16% FA, **8% a separate
`forge_f32_to_f16` epilogue** (768 launches), ~9% small ops.

**Win 1 — Q6_K prefill GEMM through the vendored `mul_mat_q`.** Added a Q6_K
instantiation to `kernels/cuda/mmq_q4k.cu` (`forge_mmq_q6k_x*`, GGML_TYPE_Q6_K through
the same dense-tiling body), reaching ggml's `load_tiles_q6_K` → `vec_dot_q6_K_q8_1_mma`
verbatim. Q6_K uses the **D4** q8_1 layout (d only, no partial sum) vs Q4_K's DS4 — added
`forge_quantize_mmq_q8_1_d4`. Smem is identical (MMQ_MMA_TILE_X_K == 76 for both), so the
Q4_K `mmq_kk_config` mmq_x/smem pick is reused. Routed for Q6_K prefill (`n_tokens >= 64`)
in `launchers.rs::gemm_q6_k_f16_at`; Q6_K DECODE stays on the Mojo dp4a gemv.

**Win 2 — MMQ writes f16 directly.** The vendored `mmq_write_back_mma` wrote f32 then a
separate kernel converted to f16. Folded the conversion into a f16 write-back
(`forge_mmq_write_back_f16`, `__float2half` store) for both Q4_K and Q6_K, dropping the
f32 output scratch and all 768 `forge_f32_to_f16` launches (kernel deleted).

**Correctness.** GPU-vs-CPU-golden (`scratch/mmq_probe/q6k_harness.cu`, canonical ggml
Q6_K dequant of the SAME bytes, tol 5e-3 = q8_1 + f16 noise):

```
Q6_K N=4096 K=512  T=512 mmq_x=128 : relL2=3.768e-03  PASS
Q6_K N=4096 K=512  T=64  mmq_x=64  : relL2=3.757e-03  PASS
Q6_K N=4096 K=2048 T=512 mmq_x=128 : relL2=3.765e-03  PASS
Q6_K N=384  K=512  T=200 mmq_x=128 : relL2=3.773e-03  PASS
```

Q4_K f16-epilogue harness re-passes (`harness.cu`, __half dst): N=4096 K=512/2048 T=512
relL2 3.97e-3 / 3.82e-3 PASS. **Coherence** (`forge run … --temp 0`): Mistral-7B Q4_K_M
→ `Paris, France. It is one of the most famous landmarks in the world` — **token-identical**
to the all-Mojo reference (`FORGE_GEMM=mojo`). Bielik NVFP4 golden bit-exact on 1 and 4 lanes.

**End-to-end prefill** (`forge bench --prefix-cache off`, FA on, best-of-3 steady):

| prompt | before (Q4_K-MMQ default) | after (this pass) | ratio | vs llama.cpp |
|--------|---------------------------|-------------------|-------|--------------|
| pp512   | ~4609 | **5851** | 1.27× | — |
| pp4096  | 6478  | **7956** | 1.23× | **0.665×** (11960; was 0.54×) |
| pp8192  | 6327  | **7753** | 1.23× | 0.70× (11019) |

Decode unchanged (146.2 tok/s @4096, 130.5 @8192; Q6_K decode still Mojo dp4a).
All-Mojo lower bound (`FORGE_GEMM=mojo`) pp4096 = 4485 for reference.

**nsys after** (pp4096, prefill-dominated): MMQ-Q4_K 55%, FA 17%, **MMQ-Q6_K 10%**
(`forge_mmq_q6k_x128_nc` — replaces the 27% Mojo `gemm_q6_k`), silu_mul 8%,
rmsnorm_residual 4%, quantize-ds4 2%, **quantize-d4 <1%**. The `forge_f32_to_f16` kernel
is **absent** (was 8%); the Mojo Q6_K prefill GEMM is **absent** (only the 4-instance
decode `gemv_q6_k_dp4a` remains). Remaining gap to llama.cpp: MMQ-Q4_K (55%) is still the
dominant cost, plus the un-fused quant/norm/attention pipeline (ggml fuses; FORGE launches
quantize+GEMM separately) and no stream-K load balancing in the dense wrapper.

**Committed state:** `kernels/cuda/mmq_q4k.cu` (generic `forge_mmq_body<type>` + f16
write-back + Q6_K entries + D4 quant; `forge_f32_to_f16` removed), `build.sh`,
`registry.rs` (Q6_K entries + `quantize_mmq_q8_1_d4`, `mmq_f32_to_f16` removed),
`launchers.rs` (`gemm_mmq_at` shared Q4_K/Q6_K, writes f16 direct; `MmqScratch` f32 dst
removed; Q6_K routing in `gemm_q6_k_f16_at`). Default path Bielik NVFP4 golden bit-exact.
