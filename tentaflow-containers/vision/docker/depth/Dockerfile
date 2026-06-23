# =============================================================================
# Plik: Dockerfile
# Opis: Kontener serwisu głębi (Depth Anything V2 / ZoeDepth / MiDaS), direct-http (bez
#       sidecara). server.py nasłuchuje HTTP na 0.0.0.0:${PORT}; Core gada HTTP
#       wprost. Oba modele przez `transformers` depth-estimation; MODEL wybiera
#       repo HF. Baza CUDA runtime (bez nvcc — brak rozszerzeń C++ do budowy).
# =============================================================================

FROM nvidia/cuda:13.0.3-cudnn-runtime-ubuntu24.04
RUN apt-get update && apt-get install -y --no-install-recommends \
      python3 python3-pip python3-venv curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*

RUN python3 -m venv /opt/depth-venv
ENV PATH="/opt/depth-venv/bin:$PATH"
RUN pip install --no-cache-dir --upgrade pip wheel

# torch+torchvision cu130 PRZED resztą, by pip nie nadpisał ich wariantem CPU.
RUN pip install --no-cache-dir \
      --extra-index-url https://download.pytorch.org/whl/cu130 \
      torch==2.11.0 torchvision==0.26.0
RUN pip install --no-cache-dir \
      "transformers>=4.45" accelerate "pillow>=12" numpy \
      fastapi==0.115.6 uvicorn==0.34.0 python-multipart==0.0.20 \
      huggingface_hub hf_transfer
ENV HF_HUB_ENABLE_HF_TRANSFER=1

COPY tentaflow-containers/vision/python/depth/server.py /app/server.py
COPY tentaflow-containers/vision/docker/depth/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

HEALTHCHECK --interval=30s --timeout=5s --start-period=300s --retries=3 \
    CMD curl -fsS "http://127.0.0.1:${PORT:-8096}/health" || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
