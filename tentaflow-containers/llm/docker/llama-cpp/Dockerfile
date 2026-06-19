# =============================================================================
# Plik: Dockerfile
# Opis: Obraz kontenera LLM inference z llama.cpp server, direct-http (bez
#       sidecara). llama-server (0.0.0.0:$PORT) serwuje OpenAI-compatible API,
#       Core gada HTTP wprost do host-mapped portu.
# =============================================================================

# --- Stage 1: llama.cpp z CUDA + runtime ---
# CUDA 13.0 (NIE 13.1 - regresja/crash MMQ w llama.cpp).
FROM nvidia/cuda:13.0.3-devel-ubuntu24.04 AS llamacpp-builder
WORKDIR /build
RUN apt-get update && apt-get install -y --no-install-recommends \
      git cmake build-essential ca-certificates \
    && rm -rf /var/lib/apt/lists/*
RUN git clone --depth 1 https://github.com/ggml-org/llama.cpp.git
WORKDIR /build/llama.cpp
# Jawna lista architektur GPU - GGML nie ma sm_103 w domyslnej liscie.
# 103-real = B300, 100-real = B200; pozostale (Turing/Ampere/Ada/Hopper/Blackwell
# consumer) dla kompatybilnosci wstecz z innymi GPU.
RUN cmake -B build -DGGML_CUDA=ON -DLLAMA_CURL=OFF -DCMAKE_BUILD_TYPE=Release \
      -DCMAKE_CUDA_ARCHITECTURES="75-real;80-real;86-real;89-real;90-real;100-real;103-real;120-real" \
    && cmake --build build --target llama-server -j"$(nproc)" \
    && strip build/bin/llama-server

# --- Stage 2: runtime ---
FROM nvidia/cuda:13.0.3-runtime-ubuntu24.04
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

COPY --from=llamacpp-builder /build/llama.cpp/build/bin/llama-server /usr/local/bin/llama-server
COPY tentaflow-containers/llm/docker/llama-cpp/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

HEALTHCHECK --interval=30s --timeout=5s --start-period=60s --retries=3 \
    CMD curl -fsS "http://127.0.0.1:${LLAMA_PORT:-8080}/health" || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
