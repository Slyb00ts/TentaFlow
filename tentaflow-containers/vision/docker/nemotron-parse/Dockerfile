# =============================================================================
# Plik: Dockerfile
# Opis: Kontener Nemotron-Parse (transformers, CUDA) + sidecar TentaFlow.
#       server.py nasluchuje HTTP na 127.0.0.1:8094, sidecar tlumaczy
#       QUIC ↔ HTTP.
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

# --- Stage 2: Nemotron-Parse na CUDA (server.py wymaga GPU) ---
FROM nvidia/cuda:13.0.3-cudnn-runtime-ubuntu24.04
RUN apt-get update && apt-get install -y --no-install-recommends \
      python3 python3-pip python3-venv python3-dev curl ca-certificates \
      git git-lfs \
    && rm -rf /var/lib/apt/lists/*

RUN python3 -m venv /opt/parse-venv
ENV PATH="/opt/parse-venv/bin:$PATH"
RUN pip install --no-cache-dir --upgrade pip wheel

# torch+torchvision cu130 dla B300 (sm_103) — osobna warstwa (cache), instalowane
# PRZED reszta zeby pip nie nadpisal ich wariantem CPU przy rozwiazywaniu zaleznosci.
RUN pip install --no-cache-dir \
      --extra-index-url https://download.pytorch.org/whl/cu130 \
      torch==2.11.0 torchvision==0.26.0
# Nemotron-Parse to model transformers (trust_remote_code) wymagajacy transformers
# >=4.51.3 (Parse architektura Eagle VLM) oraz open-clip-torch dla enkodera obrazu.
RUN pip install --no-cache-dir \
      transformers==4.51.3 "open-clip-torch>=3.3.0" pillow==11.1.0 accelerate==1.4.0 \
      einops albumentations opencv-python-headless \
      sentencepiece==0.2.0 beautifulsoup4==4.12.3 fastapi==0.115.6 uvicorn==0.34.0 python-multipart==0.0.20 \
      "huggingface_hub[cli,hf_transfer]"
ENV HF_HUB_ENABLE_HF_TRANSFER=1

# Skrypty pomocnicze parsowania (postprocessing.py, hf_logits_processor.py,
# latex2html.py) leza w repo HF obok wag i NIE sa instalowane przez transformers
# — klonujemy je do /app (pomijajac wagi LFS), server.py je importuje.
RUN git lfs install --skip-smudge \
    && git clone --depth 1 https://huggingface.co/nvidia/NVIDIA-Nemotron-Parse-v1.2 /tmp/parse-src \
    && mkdir -p /app && cp /tmp/parse-src/*.py /app/ && rm -rf /tmp/parse-src

COPY --from=sidecar-builder /tentaflow-sidecar /usr/local/bin/tentaflow-sidecar
COPY tentaflow-containers/vision/python/nemotron-parse/server.py /app/server.py
COPY tentaflow-containers/vision/docker/nemotron-parse/config.default.toml /app/config.default.toml
COPY tentaflow-containers/vision/docker/nemotron-parse/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

EXPOSE 5000/udp

HEALTHCHECK --interval=30s --timeout=5s --start-period=300s --retries=3 \
    CMD pgrep tentaflow-sidecar || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
