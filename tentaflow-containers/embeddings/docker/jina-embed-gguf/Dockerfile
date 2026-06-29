# =============================================================================
# Plik: Dockerfile
# Opis: Obraz Jina Embeddings v5 (GGUF) = llama-server CPU-only, direct-http
#       (bez sidecara). Buduje llama.cpp BEZ CUDA z GGML_NATIVE=OFF, zeby obraz
#       byl przenosny miedzy roznymi CPU (brak -march=native) i dzialal na
#       slabym sprzecie oraz AMD. Serwuje OpenAI-compatible /v1/embeddings na
#       0.0.0.0:${LLAMA_PORT}. Wariant GPU embeddingow to vLLM (`jina-embed`).
# =============================================================================

# --- Stage 1: llama.cpp CPU ---
FROM ubuntu:24.04 AS llamacpp-builder
WORKDIR /build
RUN apt-get update && apt-get install -y --no-install-recommends \
      git cmake build-essential ca-certificates \
    && rm -rf /var/lib/apt/lists/*
RUN git clone --depth 1 https://github.com/ggml-org/llama.cpp.git
WORKDIR /build/llama.cpp
# GGML_NATIVE=OFF: obraz musi dzialac na dowolnym x86-64 (build host moze miec
# inny CPU niz target). OpenMP zostaje wlaczony dla rownoleglosci na CPU.
RUN cmake -B build -DGGML_CUDA=OFF -DGGML_NATIVE=OFF -DLLAMA_CURL=OFF \
      -DCMAKE_BUILD_TYPE=Release \
    && cmake --build build --target llama-server -j"$(nproc)" \
    && strip build/bin/llama-server

# --- Stage 2: runtime ---
FROM ubuntu:24.04
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl libgomp1 \
    && rm -rf /var/lib/apt/lists/*

# Upstream llama.cpp master linkuje llama-server dynamicznie z rozdzielonymi
# .so (libllama-server-impl, libllama, libggml*, libmtmd*). Kopiujemy komplet
# bibliotek obok binarki i rejestrujemy sciezke przez ldconfig, inaczej runtime
# pada na "error while loading shared libraries: libllama-server-impl.so".
COPY --from=llamacpp-builder /build/llama.cpp/build/bin/llama-server /usr/local/bin/llama-server
COPY --from=llamacpp-builder /build/llama.cpp/build/bin/*.so /usr/local/lib/
RUN ldconfig
