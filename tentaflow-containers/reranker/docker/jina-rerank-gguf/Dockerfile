# =============================================================================
# Plik: Dockerfile
# Opis: Obraz Jina Reranker v3 (GGUF) = llama-server CPU-only, direct-http (bez
#       sidecara). Buduje llama.cpp BEZ CUDA z GGML_NATIVE=OFF (przenosny miedzy
#       CPU, dziala na slabym sprzecie i AMD). Serwuje OpenAI-compatible
#       /v1/rerank na 0.0.0.0:${LLAMA_PORT}. Wariant GPU to vLLM (`jina-rerank`).
# =============================================================================

FROM ubuntu:24.04 AS llamacpp-builder
WORKDIR /build
RUN apt-get update && apt-get install -y --no-install-recommends \
      git cmake build-essential ca-certificates \
    && rm -rf /var/lib/apt/lists/*
RUN git clone --depth 1 https://github.com/ggml-org/llama.cpp.git
WORKDIR /build/llama.cpp
RUN cmake -B build -DGGML_CUDA=OFF -DGGML_NATIVE=OFF -DLLAMA_CURL=OFF \
      -DCMAKE_BUILD_TYPE=Release \
    && cmake --build build --target llama-server -j"$(nproc)" \
    && strip build/bin/llama-server

FROM ubuntu:24.04
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl libgomp1 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=llamacpp-builder /build/llama.cpp/build/bin/llama-server /usr/local/bin/llama-server
COPY tentaflow-containers/reranker/docker/jina-rerank-gguf/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

HEALTHCHECK --interval=30s --timeout=5s --start-period=60s --retries=3 \
    CMD curl -fsS "http://127.0.0.1:${LLAMA_PORT:-8080}/health" || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
