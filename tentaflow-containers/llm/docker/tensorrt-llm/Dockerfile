# =============================================================================
# Plik: Dockerfile
# Opis: Kontener TensorRT-LLM od NVIDIA z OpenAI-compatible serwerem
#       (trtllm-serve) + sidecar QUIC TentaFlow. Wymaga karty NVIDIA z FP8
#       (H100, L40S) lub mocnej karty FP16 (A100, A6000). Konsumenckie
#       Ada/Ampere (RTX 30/40) dzialaja, ale vLLM bedzie tam efektywniejszy.
# =============================================================================

# --- Stage 1: sidecar QUIC z Rust ---
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
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/tentaflow-containers/sidecar/target \
    cargo build --release --bin tentaflow-sidecar \
    && cp target/release/tentaflow-sidecar /tentaflow-sidecar

# --- Stage 2: TensorRT-LLM runtime ---
# Oficjalny obraz NVIDIA z preinstalowanym TensorRT-LLM (Triton + Python).
# Alternatywa: nvidia/cuda:12.6-devel + pip install tensorrt_llm, ale wheel
# wazy ~8GB i wymaga dokladnej wersji CUDA, wiec zostajemy przy oficjalnym.
ARG BASE_IMAGE=nvcr.io/nvidia/tritonserver:24.08-trtllm-python-py3
FROM ${BASE_IMAGE}

RUN apt-get update && apt-get install -y --no-install-recommends \
      curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Sidecar QUIC (port 5000) — pomost miedzy mesh TentaFlow a serwerem TensorRT-LLM
COPY --from=sidecar-builder /tentaflow-sidecar /usr/local/bin/tentaflow-sidecar
COPY tentaflow-containers/llm/docker/tensorrt-llm/config.default.toml /app/config.default.toml
COPY tentaflow-containers/llm/docker/tensorrt-llm/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

EXPOSE 5000/udp

# TensorRT-LLM ma dlugi cold start (build engine moze zajac kilkanascie minut
# przy pierwszym starcie z surowego modelu HF), dlatego start_period=300s.
HEALTHCHECK --interval=30s --timeout=5s --start-period=300s --retries=3 \
    CMD pgrep tentaflow-sidecar || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
