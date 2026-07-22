# FORGE benchmarks — RTX 4090, batch 1

Date: 2026-07-17 · driver 610.43 · CUDA 13.3 · llama.cpp build 6b80c74f2 (CUDA,
`llama-bench -ngl 99`) · vLLM `vllm/vllm-openai:latest` (`vllm bench latency`,
gpu-mem-util 0.75). FORGE: `forge bench` after CUDA-graph decode landed.

## Qwen3-0.6B

| Engine | Weights | Decode tok/s | Prefill tok/s |
|---|---|---:|---:|
| llama.cpp | GGUF Q8_0 | **652** | 36 073 (batched) |
| vLLM | BF16 | ~490 ¹ | (included in ¹) |
| **FORGE** | GGUF Q8_0 | **247** | 328 (sequential ²) |

¹ vLLM latency mode reports end-to-end only: 256-in + 256-out in 0.537 s.
² FORGE has no batched prefill yet — prompt tokens run through the decode path
one at a time (PLAN chunk 6 remaining work), so prefill is not comparable.

## Bielik-PL-Minitron-7B NVFP4 (software FP4 on both — no FP4 units on sm_89)

| Engine | FP4 path | Decode tok/s |
|---|---|---:|
| vLLM | Marlin weight-only | ~165 ¹ |
| **FORGE** | Mojo fused dequant-GEMV | **91.5** |
| llama.cpp | n/a — cannot run compressed-tensors NVFP4 | — |

¹ 128-in + 128-out in 0.785 s end-to-end.

## Reading

- FORGE decode sits at **38 % of llama.cpp** (0.6B Q8) and **55 % of vLLM**
  (7B NVFP4). The dominant gap is GEMV memory bandwidth: our kernels reach
  ~140 GB/s of ~1000 GB/s peak (`kernels/mojo/bench_gemv.mojo`); llama.cpp's
  equivalents reach 700+. Fix = decomposition change (warp-per-row × multi-row
  blocks, u16 loads, smem staging of x) + autotuner — PLAN chunk 6.
- CUDA-graph decode (whole step captured once, replayed per token) raised 7B
  decode from ~69 to 91.5 tok/s (+33 %); on 0.6B the effect is smaller
  (fewer launches per step relative to kernel time).
- Prefill parity requires batched (multi-token) kernels — the single biggest
  missing piece vs both baselines.

Repro:
```
cargo run -p forge-cli --release -- bench <model> --tokens 256 --prompt-tokens 256
~/.cache/tentaflow-native-libs/src/llama.cpp/build-bench/bin/llama-bench -m <gguf> -p 256 -n 256 -ngl 99
docker run --rm --gpus all --device /dev/nvidia-uvm --device /dev/nvidiactl \
  --device /dev/nvidia0 --ipc=host -v <model>:/model --entrypoint vllm \
  vllm/vllm-openai:latest bench latency --model /model --input-len 256 \
  --output-len 256 --batch-size 1 --gpu-memory-utilization 0.75
```
(vLLM container on this host needs the explicit /dev/nvidia* device flags —
nvidia-container-toolkit does not mount nvidia-uvm here.)

## Update 2026-07-17 — GEMV v2 + CUDA-graph decode + batched prefill

| Model | Metric | FORGE before | FORGE now | llama.cpp | vLLM |
|---|---|---:|---:|---:|---:|
| Qwen3-0.6B Q8_0 | decode | 247 | **~300** | 652 | ~490 (BF16) |
| Qwen3-0.6B Q8_0 | prefill | 328 | **9 430** | 36 073 | — |
| Bielik-7B NVFP4 | decode | 69 | **112** | n/a | ~165 (Marlin) |
| Bielik-7B NVFP4 | prefill | 79 | **428** (1k prompt) | n/a | — |

What landed: warp-per-row GEMV v2 with explicit `alignment=` vector loads
(Q8_0 908 GB/s, NVFP4 649 GB/s, ~90 %/65 % of DRAM peak), whole-decode-step
CUDA-graph replay, pinned-host staging, and batched prefill
(transpose + broadcast-weight GEMM ~13 TFLOP/s + causal paged attn_prefill),
bit-identical outputs vs the per-token path on all three weight formats.

Remaining gap analysis: decode on the 0.6B is bounded by the ~200-kernel
per-step floor (fusions: QKV/gate-up concat, norm+rope+append merge); 7B NVFP4
decode by e2m1 decode ALU cost; prefill by the GEMM kernel (tensor-core/mma
path is the next step to approach llama.cpp's 36k). All tracked in PLAN chunk 6.

## Update 2026-07-17 — QKV / gate-up weight fusion (decode launch-floor cut)

Q/K/V (and gate/up) matrices are row-concatenated host-side at load into one
matrix per layer when they share a storage format (NVFP4 additionally requires
identical tensor global scales — rescaling FP8 block scales would break
bit-exactness). Decode drops from 7 to 4 GEMV launches per layer; the section
kernels (k-norm, rope-k, kv-append, silu-mul) address the fused activation
buffer by byte offset. Prefill reads q/k/v and gate/up as row-window GEMMs out
of the same fused matrix (no second weight copy in VRAM). All three formats
fuse on the test models (Bielik NVFP4 40/40 layers, Qwen3 Q8_0 28/28,
TinyLlama F16 22/22); outputs are bit-identical to the unfused path.

| Model | Decode before | Decode after |
|---|---:|---:|
| Qwen3-0.6B Q8_0 (256/256) | 302.1 | **314.8** (+4.2 %) |
| Bielik-7B NVFP4 (128/128) | 112.2 | **117.5** (+4.7 %) |

Before/after measured on the same tree (same GEMV kernels), fusion isolated
via stash. Prefill moves within noise (11.2k→11.5k / 259→264 tok/s).

## Aktualizacja 2026-07-22: hybrydowy prefill B2 T32

Model `protoLabsAI/ThinkingCap-Qwen3.6-27B-MTP-GGUF`, wariant NVFP4, RTX 4090.
Każda próba obejmuje dwa requesty, pełne porównanie ID i sampling GPU. Funkcja
jest domyślnie wyłączona; ON oznacza `FORGE_HYBRID_PREFILL_BATCH=1`, OFF oznacza
`0`. Zakres wykonawczy jest obecnie ograniczony do NVIDIA warp32 oraz dokładnie
`B=2`, `T=32`.

| Prompt | Tryb | Prefill tok/s, mediana | TTFT, mediana | E2E, mediana |
|---|---|---:|---:|---:|
| raw128 | ON | **309,5** | **827,24 ms** | **1120,08 ms** |
| raw128 | OFF | 248,6 | 1029,78 ms | 1322,54 ms |
| raw512 | ON | **320,2** | **3198,02 ms** | **3505,75 ms** |
| raw512 | OFF | 251,4 | około 4073 ms | około 4380 ms |

Pięć wyników raw512 ON to 320,5; 320,2; 319,9; 320,0; 320,2 tok/s. Każda
próba wykonała dokładnie 16 kroków B2 i obsłużyła 1024 tokeny wejściowe. Pięć
wyników raw128 ON to 309,9; 309,5; 309,4; 309,3; 309,5 tok/s; OFF to 248,6;
248,6; 248,6; 248,5; 248,6 tok/s.

Stały scratch wynosi 450 692 688 B (429,81 MiB). Profil dwóch requestów po
osiem tokenów zawiera 18 150 launchy, osiem synchronizacji i osiem transferów
D2H po 8 B, bez kopiowania logitów słownika. Catch-up MTP zachowuje atomową
transakcję pary, lecz nadal wykonuje dwa seryjne przebiegi macierzowe lane po
lane. Jest to główny pozostały koszt tej części ścieżki.

Źródła kerneli są zapisane w Mojo z myślą o portowaniu, ale pomiary i testy
wykonawcze dotyczą wyłącznie NVIDIA warp32. AMD i Metal nie są obecnie
zweryfikowanymi backendami tej optymalizacji. Surowe logi i raporty `nsys`
pozostają lokalnie w `/tmp`; nie są częścią repozytorium.
