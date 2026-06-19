# =============================================================================
# Plik: Dockerfile
# Opis: Kontener PaddleOCR-VL (GPU OCR przez transformers + torch cu130),
#       direct-http (bez sidecara). server.py nasluchuje HTTP na 0.0.0.0:8095,
#       Core gada HTTP wprost. torch cu130 ma kernele sm_103 (zgodny z B300).
# =============================================================================

# --- PaddleOCR-VL GPU (transformers + torch cu130) ---
FROM nvidia/cuda:13.0.3-cudnn-runtime-ubuntu24.04
RUN apt-get update && apt-get install -y --no-install-recommends \
      python3.12 python3-pip python3.12-venv \
      libgl1 libglib2.0-0 libgomp1 curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*

RUN python3.12 -m venv /opt/ocr-venv
ENV PATH="/opt/ocr-venv/bin:$PATH"
RUN pip install --no-cache-dir --upgrade pip wheel
RUN pip install --no-cache-dir \
      --extra-index-url https://download.pytorch.org/whl/cu130 \
      torch==2.11.0 \
    && pip install --no-cache-dir \
      transformers==4.49.0 accelerate==1.4.0 sentencepiece==0.2.0 \
      pillow==11.1.0 fastapi==0.115.6 uvicorn==0.34.0 python-multipart

COPY tentaflow-containers/vision/python/paddle-ocr/server.py /app/server.py
COPY tentaflow-containers/vision/docker/paddle-ocr/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

HEALTHCHECK --interval=30s --timeout=5s --start-period=120s --retries=3 \
    CMD curl -fsS "http://127.0.0.1:${PORT:-8095}/health" || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
