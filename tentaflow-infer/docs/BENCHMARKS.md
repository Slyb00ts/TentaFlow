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
