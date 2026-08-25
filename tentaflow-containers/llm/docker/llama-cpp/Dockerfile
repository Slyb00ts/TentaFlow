# =============================================================================
# Plik: Dockerfile
# Opis: Obraz kontenera LLM inference z llama.cpp server, direct-http (bez
#       sidecara). llama-server (0.0.0.0:$PORT) serwuje OpenAI-compatible API,
#       Core gada HTTP wprost do host-mapped portu.
#
#       Backend jest build-argiem, nie stala. Domyslnie budujemy CPU na czystej
#       Ubuntu — obraz, ktory postawi sie na KAZDYM hoscie. Wariant CUDA wchodzi
#       przez `arch_variants` w manifescie silnika: Core wykrywa arch-tag GPU
#       (`GpuSnapshot::cuda_arch_tag()`) i podmienia obrazy bazowe oraz flagi
#       cmake. Wczesniej `-DGGML_CUDA=ON` bylo wpisane na sztywno, wiec host bez
#       karty NVIDIA kompilowal ~godzine i konczyl na linkerze
#       (`libcuda.so.1 not found`, `undefined reference to cuMemCreate`) —
#       deploy nie mial prawa sie udac, a kreator i tak proponowal Dockera.
# =============================================================================

# ARG przed pierwszym FROM jest globalny; kazdy stage, ktory go uzywa, musi go
# zadeklarowac ponownie.
ARG LLAMA_BUILDER_IMAGE=ubuntu:24.04
ARG LLAMA_RUNTIME_IMAGE=ubuntu:24.04
# GGML_NATIVE=OFF: bez tego kompilator wstrzeliwuje instrukcje CPU hosta
# budujacego i obraz wywala sie SIGILL na starszej maszynie.
ARG LLAMA_CMAKE_FLAGS=-DGGML_NATIVE=OFF
# Extra apt packages per backend: a Vulkan build needs headers plus the
# shader compiler, and its runtime needs a loader and the Mesa driver.
ARG LLAMA_BUILD_PACKAGES=
ARG LLAMA_RUNTIME_PACKAGES=

# --- Stage 1: build llama.cpp ---
FROM ${LLAMA_BUILDER_IMAGE} AS llamacpp-builder
ARG LLAMA_CMAKE_FLAGS
ARG LLAMA_BUILD_PACKAGES
WORKDIR /build
RUN apt-get update && apt-get install -y --no-install-recommends \
      git cmake build-essential ca-certificates ${LLAMA_BUILD_PACKAGES} \
    && rm -rf /var/lib/apt/lists/*
RUN git clone --depth 1 https://github.com/ggml-org/llama.cpp.git
WORKDIR /build/llama.cpp
RUN cmake -B build ${LLAMA_CMAKE_FLAGS} -DLLAMA_CURL=OFF -DCMAKE_BUILD_TYPE=Release \
    && cmake --build build --target llama-server -j"$(nproc)" \
    && strip build/bin/llama-server

# --- Stage 2: runtime ---
FROM ${LLAMA_RUNTIME_IMAGE}
ARG LLAMA_RUNTIME_PACKAGES
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl libgomp1 ${LLAMA_RUNTIME_PACKAGES} \
    && rm -rf /var/lib/apt/lists/*

COPY --from=llamacpp-builder /build/llama.cpp/build/bin/llama-server /usr/local/bin/llama-server
# Backend GPU zostawia obok llama-server swoje wspoldzielone biblioteki ggml
# (libggml-cuda.so itd.); przy buildzie CPU katalog zawiera tylko rdzen.
COPY --from=llamacpp-builder /build/llama.cpp/build/bin/*.so* /usr/local/lib/
RUN ldconfig
COPY tentaflow-containers/llm/docker/llama-cpp/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

HEALTHCHECK --interval=30s --timeout=5s --start-period=60s --retries=3 \
    CMD curl -fsS "http://127.0.0.1:${LLAMA_PORT:-8080}/health" || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
