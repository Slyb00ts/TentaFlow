# =============================================================================
# Plik: Dockerfile
# Opis: Obraz zamiennika VLM Qwen3-VL = vLLM (PyPI wheels) w trybie
#       generacyjnym chat z obsluga obrazow + sidecar TentaFlow. Model przyjmuje
#       tekst i obrazy, generuje tekst; vLLM nasluchuje OpenAI API na
#       127.0.0.1:8000 (/v1/chat/completions), sidecar tlumaczy QUIC.
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
RUN cargo build --release --bin tentaflow-sidecar \
    && cp target/release/tentaflow-sidecar /tentaflow-sidecar

# --- Stage 2: vLLM z PyPI ---
# CUDA 13.0 + cudnn runtime — vLLM 0.23.0 wymaga PyTorch 2.11 + cu130. Runtime
# variant (nie devel) bo nie kompilujemy z source — wheels z PyPI maja prebuilt
# extensions (vllm, xformers, flashinfer).
FROM nvidia/cuda:13.0.3-cudnn-runtime-ubuntu24.04
RUN apt-get update && apt-get install -y --no-install-recommends \
      python3 python3-pip python3-venv python3-dev curl ca-certificates \
      gcc g++ \
    && rm -rf /var/lib/apt/lists/*

RUN python3 -m venv /opt/vllm-venv
ENV PATH="/opt/vllm-venv/bin:$PATH"
RUN pip install --no-cache-dir --upgrade pip wheel

# vLLM 0.23.0 + torch 2.11 z cu130 index. torch idzie z pytorch.org (PyPI ma
# wariant CPU). vLLM wymaga jawnego wheela +cu130 dla B300 (sm_103) — domyslny
# wheel PyPI to cu129, dlatego bierzemy go z indeksu nightly cu130 vLLM.
RUN pip install --no-cache-dir \
      --extra-index-url https://download.pytorch.org/whl/cu130 \
      torch==2.11.0 torchvision==0.26.0 torchaudio==2.11.0 \
    && pip install --no-cache-dir \
      --extra-index-url https://wheels.vllm.ai/nightly/cu130 \
      vllm==0.23.0

# hf CLI do pobierania zrodel safetensors z HF.
# hf_transfer = szybsze rownolegle pobieranie.
RUN pip install --no-cache-dir "huggingface_hub[cli,hf_transfer]"
ENV HF_HUB_ENABLE_HF_TRANSFER=1

COPY --from=sidecar-builder /tentaflow-sidecar /usr/local/bin/tentaflow-sidecar
COPY tentaflow-containers/llm/docker/qwen3-vl/config.default.toml /app/config.default.toml
COPY tentaflow-containers/llm/docker/qwen3-vl/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

EXPOSE 5000/udp

HEALTHCHECK --interval=30s --timeout=5s --start-period=300s --retries=3 \
    CMD pgrep tentaflow-sidecar || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
