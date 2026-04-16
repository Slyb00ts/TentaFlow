# =============================================================================
# Plik: Dockerfile
# Opis: Kontener Reranker (BGE-reranker, cross-encoder) — uzywa tej samej
#       infrastruktury text-embeddings-inference (TEI obsluguje rerank).
# =============================================================================

FROM rust:slim-bookworm AS sidecar-builder
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

FROM ghcr.io/huggingface/text-embeddings-inference:cuda-1.5
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
COPY --from=sidecar-builder /tentaflow-sidecar /usr/local/bin/tentaflow-sidecar
COPY tentaflow-containers/reranker/docker/bge-reranker/config.default.toml /app/config.default.toml
COPY tentaflow-containers/reranker/docker/bge-reranker/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

EXPOSE 5000/udp
HEALTHCHECK --interval=30s --timeout=5s --start-period=60s --retries=3 \
    CMD pgrep tentaflow-sidecar || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
