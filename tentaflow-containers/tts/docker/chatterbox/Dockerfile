# =============================================================================
# Plik: Dockerfile
# Opis: Sidecar QUIC + chatterbox TTS server. Sidecar tlumaczy iroh requesty na
#       lokalny HTTP 8087 gdzie nasluchuje uvicorn server.py.
# =============================================================================

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

FROM python:3.11-slim

ENV PIP_NO_CACHE_DIR=1 \
    PYTHONUNBUFFERED=1 \
    HF_HOME=/app/cache/huggingface

RUN apt-get update && apt-get install -y --no-install-recommends \
        libsndfile1 ffmpeg ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY tentaflow-containers/tts/docker/chatterbox/requirements.txt .
RUN pip install --upgrade pip && pip install -r requirements.txt
RUN python -c "from chatterbox.mtl_tts import ChatterboxMultilingualTTS; \
ChatterboxMultilingualTTS.from_pretrained(device='cpu')"
COPY tentaflow-containers/tts/docker/chatterbox/server.py /app/server.py

COPY --from=sidecar-builder /tentaflow-sidecar /usr/local/bin/tentaflow-sidecar
COPY tentaflow-containers/tts/docker/chatterbox/config.default.toml /app/config.default.toml
COPY tentaflow-containers/tts/docker/chatterbox/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

EXPOSE 5000/udp
HEALTHCHECK --interval=30s --timeout=5s --start-period=120s --retries=3 \
    CMD pgrep tentaflow-sidecar || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
