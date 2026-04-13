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
FROM rust:1.83-slim-bookworm AS sidecar-builder
WORKDIR /build
RUN apt-get update && apt-get install -y --no-install-recommends \
      pkg-config libssl-dev cmake build-essential \
    && rm -rf /var/lib/apt/lists/*

COPY tentaflow-protocol /build/tentaflow-protocol
COPY tentaflow-containers/sidecar /build/tentaflow-containers/sidecar

WORKDIR /build/tentaflow-containers/sidecar
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/tentaflow-containers/sidecar/target \
    cargo build --release --bin tentaflow-sidecar \
    && cp target/release/tentaflow-sidecar /tentaflow-sidecar

# --- Stage 2: llama.cpp z CUDA + runtime ---
FROM nvidia/cuda:12.6.3-devel-ubuntu24.04 AS llamacpp-builder
WORKDIR /build
RUN apt-get update && apt-get install -y --no-install-recommends \
      git cmake build-essential ca-certificates \
    && rm -rf /var/lib/apt/lists/*
RUN git clone --depth 1 https://github.com/ggml-org/llama.cpp.git
WORKDIR /build/llama.cpp
RUN cmake -B build -DGGML_CUDA=ON -DLLAMA_CURL=OFF -DCMAKE_BUILD_TYPE=Release \
    && cmake --build build --target llama-server -j"$(nproc)" \
    && strip build/bin/llama-server

# --- Stage 3: runtime ---
FROM nvidia/cuda:12.6.3-runtime-ubuntu24.04
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=llamacpp-builder /build/llama.cpp/build/bin/llama-server /usr/local/bin/llama-server
COPY --from=sidecar-builder /tentaflow-sidecar /usr/local/bin/tentaflow-sidecar
COPY tentaflow-containers/llm-llamacpp/config.default.toml /app/config.default.toml
COPY tentaflow-containers/llm-llamacpp/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

# QUIC port dla routera (UDP), llama-server HTTP nasluchuje tylko na localhost.
EXPOSE 5000/udp

HEALTHCHECK --interval=30s --timeout=5s --start-period=60s --retries=3 \
    CMD pgrep tentaflow-sidecar || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
