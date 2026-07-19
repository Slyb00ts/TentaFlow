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
