# =============================================================================
# Plik: Dockerfile
# Opis: Kontener detektorow dokumentowych YOLOX (NVIDIA NeMo Retriever) z torch
#       cu130 (PyTorch GPU) + sidecar TentaFlow. Wspolny obraz dla trzech modeli
#       (page-elements, graphic-elements, table-structure) - konkretny model
#       wybiera zmienna MODEL_REPO. Serwer laduje wagi .pth do sieci YOLOX i liczy
#       inferencje BEZPOSREDNIO w PyTorch na GPU (custom API /detect). onnxruntime
#       NIE jest instalowany — B300 (sm_103) nie ma kerneli onnxruntime-gpu,
#       a torch cu130 wspiera sm_103.
# Przykład: docker build -f vision/docker/nemotron-yolox/Dockerfile .
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
RUN cargo build --release --bin tentaflow-sidecar \
    && cp target/release/tentaflow-sidecar /tentaflow-sidecar

FROM nvidia/cuda:13.0.3-cudnn-runtime-ubuntu24.04
RUN apt-get update && apt-get install -y --no-install-recommends \
      python3 python3-pip python3-venv git curl ca-certificates \
      libgl1 libglib2.0-0 \
    && rm -rf /var/lib/apt/lists/*

RUN python3 -m venv /opt/venv
ENV PATH="/opt/venv/bin:$PATH"
# torch cu130 serwuje inferencje YOLOX bezposrednio na GPU (wspiera sm_103/B300).
# YOLOX dostarcza definicje sieci do zaladowania wag .pth. Bez onnxruntime.
RUN pip install --no-cache-dir --upgrade pip wheel \
 && pip install --no-cache-dir --index-url https://download.pytorch.org/whl/cu130 \
      torch torchvision \
 && pip install --no-cache-dir \
      fastapi uvicorn python-multipart \
      opencv-python-headless pillow numpy \
      huggingface_hub \
      "yolox @ git+https://github.com/Megvii-BaseDetection/YOLOX.git"

COPY --from=sidecar-builder /tentaflow-sidecar /usr/local/bin/tentaflow-sidecar
COPY tentaflow-containers/vision/python/nemotron-yolox/server.py /app/server.py
COPY tentaflow-containers/vision/python/nemotron-yolox/export_onnx.py /app/export_onnx.py
COPY tentaflow-containers/vision/docker/nemotron-yolox/config.default.toml /app/config.default.toml
COPY tentaflow-containers/vision/docker/nemotron-yolox/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

ENV MODEL_DIR=/data/models \
    NEMOTRON_YOLOX_PORT=8086

EXPOSE 5000/udp

HEALTHCHECK --interval=30s --timeout=5s --start-period=180s --retries=3 \
    CMD pgrep tentaflow-sidecar || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
