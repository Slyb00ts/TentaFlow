# tentaflow-wrappers

Crate centralizuje własne wrappery TentaFlow dla natywnych silników takich jak `llama.cpp`, `whisper.cpp` i `sherpa-onnx`.

Cel jest praktyczny: ograniczyć zależność od high-level wrapperów, które nie nadążają za upstreamem. W szczególności `llama-cpp-2` nie wystawia dziś wszystkiego, czego potrzebujemy dla lokalnego LLM.

## Stan obecnego llama.cpp runtime

Adapter `tentaflow-core/src/inference/llamacpp.rs` używa tego crate'a dla lokalnego GGUF runtime.
Obsługiwane są:

- inicjalizacji backendu `llama.cpp`;
- opcjonalnego wyciszenia logów llama.cpp dla narzędzi testowych;
- szybkiej inspekcji metadanych GGUF bez ładowania wag;
- ładowania modelu GGUF z `n_gpu_layers`;
- metadanych modelu: `n_params`, `size`, `n_ctx_train`, `n_vocab`, `n_embd`;
- tokenizacji promptu z BOS;
- kontekstu z `ctx_size` i `batch_size`;
- batch/decode dla promptu i kolejnych tokenów;
- samplerów: repeat penalty, top-k, top-p, temperature, greedy/dist;
- dekodowania tokenu do UTF-8;
- wykrywania tokenu końca generowania;
- stop sequences;
- streamingu tokenów;
- embeddingów przez `llama_get_embeddings_seq`;
- `ngram-simple` speculative decoding (`size_ngram`, `size_mgram`).

## MTP / NextN

MTP jest traktowane jako właściwość modelu GGUF, nie jako osobny draft model. Inspektor GGUF i runtime wykrywają je po metadanych `*.nextn_predict_layers`.

Aktualne ograniczenie: publiczne C API llama.cpp nie wystawia jeszcze prostej funkcji do wykonania wbudowanych NextN/MTP headów jako draft. Dlatego wrapper wykrywa MTP i nie udaje osobnego draft modelu. Pełne wykonanie MTP wymaga cienkiego C++ bridge'a albo nowej funkcji upstream w `llama.h`.

## Smoke test

```bash
cargo run --manifest-path tentaflow-wrappers/Cargo.toml \
  --features llama \
  --example llama_smoke \
  -- --model /mnt/d/models/Qwen3.5-0.8B-Q4_0.gguf --max-tokens 24

cargo run --manifest-path tentaflow-wrappers/Cargo.toml \
  --features llama \
  --example llama_smoke \
  -- --model /mnt/d/models/minitron/gguf/Qwen3.6-27B-MTP-Q4_K_M.gguf --metadata-only
```

## Strategia migracji

1. Ten crate definiuje stabilne typy konfiguracji i mapowanie `native-libs`.
2. Adapter `llamacpp` w `tentaflow-core` używa już API z tego crate'a.
3. Gdy potrzebujemy funkcji niewystawionej przez C API, dodajemy własny cienki C/C++ bridge do tego crate'a, zamiast łatać `llama-cpp-2`.
