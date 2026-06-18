# =============================================================================
# Plik: Dockerfile
# Opis: Obraz kontenera LLM inference z llama.cpp server + TentaFlow sidecar.
#       Dwa procesy w kontenerze: llama-server (localhost:8080) serwuje
#       OpenAI-compatible API, sidecar (0.0.0.0:5000) tlumaczy QUIC↔HTTP.
#
#       Build kontekst: root repo TentaFlow/, zeby sidecar mial dostep do
#       tentaflow-protocol/ i tentaflow-containers/sidecar/.
# =============================================================================

# --- Stage 1: build sidecar z Rust ---
FROM rust:slim-bookworm AS sidecar-builder
WORKDIR /build
RUN apt-get update && apt-get install -y --no-install-recommends \
      pkg-config libssl-dev cmake build-essential \
    && rm -rf /var/lib/apt/lists/*

COPY tentaflow-protocol /build/tentaflow-protocol
COPY tentaflow-transport /build/tentaflow-transport
COPY vendor /build/vendor
COPY tentaflow-containers/sidecar /build/tentaflow-containers/sidecar

WORKDIR /build/tentaflow-containers/sidecar
RUN cargo build --release --bin tentaflow-sidecar \
    && cp target/release/tentaflow-sidecar /tentaflow-sidecar

# --- Stage 2: llama.cpp z CUDA + runtime ---
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

# --- Stage 3: runtime ---
FROM nvidia/cuda:13.0.3-runtime-ubuntu24.04
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=llamacpp-builder /build/llama.cpp/build/bin/llama-server /usr/local/bin/llama-server
COPY --from=sidecar-builder /tentaflow-sidecar /usr/local/bin/tentaflow-sidecar
COPY tentaflow-containers/llm/docker/llama-cpp/config.default.toml /app/config.default.toml
COPY tentaflow-containers/llm/docker/llama-cpp/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

# QUIC port dla routera (UDP), llama-server HTTP nasluchuje tylko na localhost.
EXPOSE 5000/udp

HEALTHCHECK --interval=30s --timeout=5s --start-period=60s --retries=3 \
    CMD pgrep tentaflow-sidecar || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
