# =============================================================================
# Plik: Dockerfile
# Opis: Kontener LLM z vLLM 0.20.0 (PyPI wheels) + sidecar TentaFlow.
#       vLLM nasluchuje OpenAI API na 127.0.0.1:8000, sidecar tlumaczy QUIC.
# =============================================================================

# --- Stage 1: sidecar z Rust ---
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

# --- Stage 2: vLLM z PyPI ---
# CUDA 13.0 + cudnn runtime — vLLM 0.20.0 wymaga PyTorch 2.11 + cu130.
# Runtime variant (nie devel) bo nie kompilujemy z source — wheels z PyPI maja
# prebuilt extensions (vllm, xformers, flashinfer).
FROM nvidia/cuda:13.0.3-cudnn-runtime-ubuntu24.04
RUN apt-get update && apt-get install -y --no-install-recommends \
      python3 python3-pip python3-venv python3-dev curl ca-certificates \
      gcc g++ \
    && rm -rf /var/lib/apt/lists/*

RUN python3 -m venv /opt/vllm-venv
ENV PATH="/opt/vllm-venv/bin:$PATH"
RUN pip install --no-cache-dir --upgrade pip wheel

# vLLM 0.20.0 + torch 2.11 z cu130 index (PyPI ma vllm wheel, ale torch
# musimy wziac z pytorch.org bo PyPI ma wariant CPU).
RUN pip install --no-cache-dir \
      --extra-index-url https://download.pytorch.org/whl/cu130 \
      torch==2.11.0 torchvision==0.26.0 torchaudio==2.11.0 \
    && pip install --no-cache-dir vllm==0.20.0

COPY --from=sidecar-builder /tentaflow-sidecar /usr/local/bin/tentaflow-sidecar
COPY tentaflow-containers/llm/docker/vllm/config.default.toml /app/config.default.toml
COPY tentaflow-containers/llm/docker/vllm/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

EXPOSE 5000/udp

HEALTHCHECK --interval=30s --timeout=5s --start-period=300s --retries=3 \
    CMD pgrep tentaflow-sidecar || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
