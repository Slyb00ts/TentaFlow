# =============================================================================
# Plik: Dockerfile
# Opis: Kontener ComfyUI (image generation) + sidecar TentaFlow.
# =============================================================================

FROM rust:slim-bookworm AS sidecar-builder
WORKDIR /build
RUN apt-get update && apt-get install -y --no-install-recommends \
      pkg-config libssl-dev cmake build-essential \
    && rm -rf /var/lib/apt/lists/*
COPY tentaflow-protocol /build/tentaflow-protocol
COPY tentaflow-transport /build/tentaflow-transport
COPY tentaflow-containers/sidecar /build/tentaflow-containers/sidecar
WORKDIR /build/tentaflow-containers/sidecar
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/tentaflow-containers/sidecar/target \
    cargo build --release --bin tentaflow-sidecar \
    && cp target/release/tentaflow-sidecar /tentaflow-sidecar

FROM nvidia/cuda:12.6.3-runtime-ubuntu24.04
RUN apt-get update && apt-get install -y --no-install-recommends \
      python3 python3-pip python3-venv python3-dev git curl ca-certificates \
      libgl1 libglib2.0-0 gcc g++ \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /opt
RUN git clone --depth 1 https://github.com/comfyanonymous/ComfyUI.git
WORKDIR /opt/ComfyUI

RUN python3 -m venv /opt/venv
ENV PATH="/opt/venv/bin:$PATH"
RUN pip install --no-cache-dir --upgrade pip wheel \
 && pip install --no-cache-dir torch torchvision torchaudio \
      --extra-index-url https://download.pytorch.org/whl/cu124 \
 && pip install --no-cache-dir -r requirements.txt

COPY --from=sidecar-builder /tentaflow-sidecar /usr/local/bin/tentaflow-sidecar
COPY tentaflow-containers/image-gen/docker/comfyui/config.default.toml /app/config.default.toml
COPY tentaflow-containers/image-gen/docker/comfyui/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

EXPOSE 5000/udp
HEALTHCHECK --interval=30s --timeout=5s --start-period=120s --retries=3 \
    CMD pgrep tentaflow-sidecar || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
