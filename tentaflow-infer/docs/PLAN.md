# FORGE — Implementation Plan (tentaflow-infer)

Universal multi-vendor AI inference engine. Rust (systems) + Mojo (GPU kernels).
Spec: `docs/SPEC.md` (v0.1). This plan maps the spec onto concrete crates and
implementation chunks, ordered so that every chunk ends in a working, testable state.

## Ground truth for this machine (first target)

- GPU: RTX 4090 (24 GB, SM 8.9, no FP4/FP8 tensor cores → NVFP4/FP8 take the
  software-dequant path from day one, exactly the spec's "programowa" path).
- CUDA 13.3 (nvcc + NVRTC available), driver 610.43.
- Models available in `../.runtime/models/`:
  - `model.gguf` — qwen3-arch embedding model (GGUF v3, Q8) → GGUF loader smoke test.
  - `models--TentaFlow--Bielik-1.5B-NVFP4` — safetensors NVFP4 → NVFP4 software path e2e.
  - `models--TentaFlow--Bielik-PL-Minitron-7B-NVFP4`, `Bielik-Minitron-7B FP8` — scale-up.
- Mojo: installed via pixi if available; the kernel registry always keeps a
  CUDA-C/NVRTC baseline slot per op (the spec's sanctioned safety slot), so the
  engine is never blocked on Mojo toolchain maturity. Mojo kernels replace
  baseline slots op-by-op behind the same registry key with golden tests.

## Workspace layout

```
tentaflow-infer/
├── Cargo.toml                 # workspace
├── crates/
│   ├── forge-types            # DType, shapes, errors, DeviceCaps, quant enums
│   ├── forge-hal              # Device/Stream/Event/Collectives traits + backends
│   │                          #   cuda (cudarc, NVRTC JIT + PTX cache), cpu (rayon)
│   ├── forge-formats          # GGUF (mmap zero-copy, all K/IQ quants), safetensors,
│   │                          #   HF config.json, architecture registry (declarative)
│   ├── forge-tokenize         # HF tokenizers + GGUF-embedded tokenizer reconstruction
│   ├── forge-kernels          # kernel registry (op,dtype,quant,arch,shape-bucket → impl)
│   │                          #   sources in kernels/cuda/*.cu (NVRTC) and kernels/mojo/
│   ├── forge-graph            # light op-graph IR + memory planner (activations ring)
│   ├── forge-engine           # LLM engine: model exec, paged KV, sampling,
│   │                          #   continuous batching scheduler, chunked prefill,
│   │                          #   prefix cache (radix), speculation framework
│   ├── forge-server           # axum: OpenAI API (+Anthropic later), SSE, minijinja
│   │                          #   chat templating, tool-call parsers, admission control
│   └── forge-cli              # forge pull|run|serve|bench
├── kernels/
│   ├── cuda/                  # .cu baseline kernels (NVRTC-compiled, PTX-cached)
│   └── mojo/                  # Mojo kernels (same registry keys), gated by toolchain
└── docs/ (SPEC.md, PLAN.md, adr/)
```

## Chunks (each ends buildable + tested + codex-reviewed)

### Chunk 0 — scaffold & foundations  ✅ exit: workspace builds, CI-able
Workspace, forge-types, ADR-0001 (Mojo gate policy), ADR-0002 (NVRTC JIT + PTX
cache instead of build-time nvcc).

### Chunk 1 — forge-hal: CUDA + CPU backends
- `Device` trait per spec §3.1: alloc (device/pinned/managed) with arena/slab pools
  (no cudaMalloc in hot path), streams, events, H2D/D2H/D2D copies, kernel launch,
  `DeviceCaps` (smem, dtypes, fp8/fp4 native flags, P2P).
- CUDA backend on cudarc (dynamic loading; works with CUDA 13), NVRTC kernel
  compilation with on-disk PTX cache keyed by (kernel-src-hash, arch, opts).
- Graph capture API surface (CUDA Graphs) — capture/replay wired, used from Chunk 6.
- CPU backend: rayon + aligned buffers (reference + fallback).
- Tests: alloc/copy roundtrip, saxpy via NVRTC, caps report on 4090.

### Chunk 2 — forge-formats: GGUF + safetensors + arch registry
- GGUF v2/v3 parser: mmap, zero-copy tensor views, full metadata typing, all
  quant types recognized (F32/F16/BF16, Q4_0..Q8_1, Q2_K..Q8_K, IQ1..IQ4, MXFP4).
- CPU reference dequant for every supported quant (golden source for GPU kernels).
- safetensors mmap loader + HF `config.json` parsing.
- NVFP4 (compressed-tensors format: e2m1 packed + FP8-E4M3 block scales /16 +
  tensor scale) — layout decode + CPU reference dequant.
- Architecture registry: declarative RON mapping tensor names → model graph roles
  (day-1: qwen3, llama, bielik/mistral-class).
- Tests: parse `.runtime/model.gguf` (310 tensors), parse Bielik-1.5B-NVFP4,
  dequant golden vs f32 references.
- ONNX loader (SPEC §4.1, opset 17+ subset) ✅ `forge-onnx`: own protobuf
  wire-format parser → typed graph IR (nodes/edges/initializers/subgraphs) +
  hybrid CPU/GPU interpreter. Heavy ops (Conv1d, LSTM, Relu/Sigmoid/Sqrt, Add,
  Pow, ReduceMean) run as native Mojo f32 kernels (`onnx_ops.mojo`); shape/
  control ops (Shape/Gather/Slice/Concat/Reshape/Transpose/Pad/Cast/Equal/Not/If
  with sr + state-init subgraphs) on host. **Gate #5 (numeric):** Silero VAD
  (`silero_vad.onnx`) on the RTX 4090 matches onnxruntime within |Δ|~1e-6 (tol
  1e-3) — sine 0.2987515 vs 0.2987524, silence 0.0442625 vs 0.0442627. CLI:
  `forge onnx-run`. Depth/embeddings ONNX parse; add ops per model via `dispatch`.

### Chunk 3 — forge-tokenize + chat templating
- HF `tokenizers` for tokenizer.json models; GGUF-embedded (gpt2/BPE + merges)
  reconstruction into the same API.
- Incremental UTF-8-safe detokenizer (byte-fallback buffer; emoji/CJK tests).
- minijinja chat templating in HF-compat mode (pycompat methods, tojson,
  raise_exception), source priority: request override → tokenizer_config →
  GGUF metadata → built-in registry (ChatML, Llama-3, Mistral, Qwen).

### Chunk 4 — forge-kernels: baseline CUDA kernel set (registry-driven)
Registry: `(op, dtype, quant, arch, shape-bucket) → KernelImpl` with slots for
multiple implementations + microbench autotune cache.
Kernels (CUDA baseline, NVRTC):
- rmsnorm (+fused residual), rope (neox/gpt-j variants), silu-mul, elementwise.
- GEMV/GEMM fused-dequant: Q8_0, Q4_K, Q6_K, NVFP4 (software path), FP16/BF16
  (plus cuBLASLt vendor-slot as the sanctioned GEMM safety net for big batch).
- Paged attention: flash-style decode (GQA/MQA/MHA) + prefill attention.
- Sampling on GPU: temperature/top-k/top-p/min-p + penalties; argmax fast path.
- Golden tests: every kernel vs CPU reference (forge-formats dequant + ndarray
  reference math), tolerances per dtype.

### Chunk 5 — forge-engine v0: single-sequence e2e
- Model builder: arch registry + weights → layer plan (qwen3 first: RMSNorm,
  GQA + QK-norm, RoPE, SwiGLU, tied/untied lm_head).
- Paged KV cache (16-64 token pages), FP16 first; KV quant ladder later.
- Blocking + streaming generate; stop sequences with holdback.
- **Exit: Bielik-1.5B-NVFP4 and a qwen3 GGUF produce coherent text on the 4090;
  logits match CPU reference within tolerance on a fixed prompt.**

### Chunk 6 — continuous batching + chunked prefill + CUDA graphs
Status: iteration-level scheduler with chunked prefill + KV-projection
admission landed in `forge-engine::server` (sequences interleave per token;
kernel-level batching pending). Measured on the 4090: fused GEMV kernels reach
~140 GB/s of ~1000 GB/s peak (Bielik-7B-NVFP4 decodes at ~55 tok/s, batch 1).
Naive SIMD-load vectorization REGRESSED both quant GEMVs (Q8_0 34-byte blocks
are 2-byte aligned → wide loads scalarize; NVFP4 int32×8 decode adds register
pressure) — the BW gap needs a decomposition change (warp-per-row × multi-row
blocks, u16-based loads, shared-memory x staging) driven by the autotuner, not
wider loads alone. `kernels/mojo/bench_gemv.mojo` is the measurement harness.
- Iteration-level scheduler: decode batch + prefill chunks under a token budget;
  SLO dual queue (latency/throughput classes); admission control with KV
  projection (429 instead of OOM).
- Batch-bucket CUDA graph capture for decode steps (1,2,4,...,64).
- Radix-tree prefix cache ✅ (`forge-engine/src/prefix.rs`, SPEC §5.2): page-
  granular tree, borrow longest shared prefix before prefill + donate own prefill
  pages on completion, LRU eviction of refcount-0 leaves, `--prefix-cache on|off`,
  `cached_tokens` in usage. Whole-page sharing (borrowers never write a shared
  page) makes partial-boundary CoW unnecessary. Proof on the 4090: shared 2048-tok
  prefix → cache_read 2016, prefill 4.7× faster, bit-identical to cold and to OFF;
  multi-turn reuse; Bielik NVFP4 golden ids unchanged with OFF (`tests/prefix_cache.rs`).
- Bench harness: tok/s prefill/decode vs llama.cpp on the same GGUF.

### Chunk 7 — forge-server: OpenAI API + CLI
- `/v1/chat/completions`, `/v1/completions`, `/v1/models`, `/v1/embeddings`
  (embedding GGUF already in .runtime), SSE streaming, usage accounting.
- Tool-call parser registry (Hermes/Qwen `<tool_call>`, Llama-3 JSON) —
  streaming incremental; `reasoning_content` extraction (`<think>`).
- logit_bias, min_tokens, n (multiple completions; shared prompt prefix via radix
  cache), seed, stop, logprobs/top_logprobs, echo — DONE (`tests/e2e_generation.rs`;
  feature-gated CPU sampler, no-feature GPU path bit-identical, Bielik golden unchanged).
- forge-cli: `serve`, `run` (interactive), `bench`, `pull` (HF Hub).

### Chunk 8 — fundament spekulacji i komponowalne propozery
- **Zrealizowane:** liniowy `NgramProposer` jest podłączony do pętli decode dla
  greedy. Jeden forward mini-prefill weryfikuje draft, a odrzucone KV są wycofywane
  przez `KvCache::rollback`. Ścieżka jest domyślnie wyłączona i ograniczona do
  `--speculative on|off|ngram:<k>`. Dowody: `tests/e2e_speculative.rs`
  (powtarzalny prompt około 1.5×, wynik zgodny z OFF) i `tests/kv_rollback.rs`.
- **Zrealizowane:** wspólny `Proposer`, typowane `DraftTree`/`DraftNode`
  (`proposal_logprob`, `conditional_confidence`, `source`), walidacja topologii,
  `SpeculationCoordinator`, kaskadowa kompozycja liniowa oraz statystyki per
  proposer. Rozgałęzione propozycje są reprezentowalne, lecz bieżący verifier
  odrzuca je jako `Unsupported`.
- **Zrealizowane:** parser i walidator `forge-speculation.json`, w tym schemat
  targetu/tensorów/kalibracji/licencji, limity wejścia, bezpieczne ścieżki względne,
  SHA-256 i zachowanie zweryfikowanych uchwytów artefaktów. Podłączenie manifestu do runtime neuralnego pozostaje do
  wykonania.
- **Do realizacji:** jedna lossless weryfikacja drzewa z greedy i stochastic
  acceptance, tree-attention i zatwierdzaniem wyłącznie zaakceptowanych KV;
  statystyki i adaptacyjne wyłączanie per proposer.
- **Do realizacji po pozyskaniu zgodnych wag:** `DraftModelProposer`,
  `MTPProposer`, `Eagle3Proposer`, `DFlashProposer`, `DSparkProposer` i opcjonalny
  `PardProposer`/`SuffixProposer`. DSpark obejmuje półautoregresyjny backbone,
  głowę Markova lub RNN, confidence head, kalibrację STS oraz scheduler długości
  weryfikacji zależny od obciążenia. Konfiguracje pięciu pierwszych proposerów są
  typowane, ale zwracają `Unsupported`, dopóki ich implementacje i wagi nie są
  dostępne.

### Chunk 9+ — later phases (tracked, not this milestone)
KV quant ladder (FP8→INT8→NVFP4-KV→rotational 3-bit), TierManager (expert
streaming + KV SSD chunked layout + persistent sessions), FORGE-RPC QUIC/CBOR,
Anthropic API, multi-GPU TP/PP + ForgeCCL, other modalities (STT/TTS/T2I),
ROCm/Metal/Level-Zero backends, Realtime API. Each amends this plan when opened.

## Mojo policy (ADR-0001 summary)

Spec policy stands: 100% kernels in Mojo, one codebase, multi-target. On this
machine the Mojo toolchain is being provisioned; until `mojo` compiles our
kernel set to PTX with parity vs the CUDA baseline, the baseline occupies the
registry slot (the spec's own sanctioned safety slot). `kernels/mojo/` carries
the canonical kernel sources as they are ported; the registry key and golden
tests are identical, so swapping implementation is a config change, not a
refactor. Gate reviews: end of each chunk ≥5.

## Quality gates (every chunk)

1. `cargo build --workspace && cargo test --workspace` green.
2. Golden numerics vs CPU reference (tolerance per dtype).
3. codex review of the diff (gpt-5.6-sol) — findings fixed before next chunk.
4. No stubs/TODO/placeholder code (repo rule).
5. Every new/changed user-facing parameter (CLI flag, API field, env var) is
   documented in `docs/INFER_CONFIGURATION.md` IN THE SAME COMMIT.
