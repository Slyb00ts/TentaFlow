# =============================================================================
# Plik: Dockerfile
# Opis: kokoro TTS server — direct-http (no sidecar). uvicorn server.py
#       nasluchuje na 0.0.0.0:${PORT}.
# =============================================================================

FROM python:3.11-slim

ENV PIP_NO_CACHE_DIR=1 \
    PYTHONUNBUFFERED=1 \
    HF_HOME=/app/cache/huggingface

RUN apt-get update && apt-get install -y --no-install-recommends \
        libsndfile1 espeak-ng ffmpeg ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY tentaflow-containers/tts/docker/kokoro/requirements.txt .
RUN pip install --upgrade pip && pip install -r requirements.txt
RUN python -c "import os; os.makedirs('/app/models', exist_ok=True); \
from huggingface_hub import hf_hub_download; \
hf_hub_download('onnx-community/Kokoro-82M-v1.0-ONNX', 'onnx/model.onnx', local_dir='/app/models'); \
hf_hub_download('onnx-community/Kokoro-82M-v1.0-ONNX', 'voices/af_heart.bin', local_dir='/app/models'); \
hf_hub_download('onnx-community/Kokoro-82M-v1.0-ONNX', 'voices/af_bella.bin', local_dir='/app/models'); \
hf_hub_download('onnx-community/Kokoro-82M-v1.0-ONNX', 'voices/am_michael.bin', local_dir='/app/models'); \
hf_hub_download('onnx-community/Kokoro-82M-v1.0-ONNX', 'voices/am_adam.bin', local_dir='/app/models'); \
hf_hub_download('onnx-community/Kokoro-82M-v1.0-ONNX', 'voices/bf_alice.bin', local_dir='/app/models'); \
hf_hub_download('onnx-community/Kokoro-82M-v1.0-ONNX', 'voices/bm_george.bin', local_dir='/app/models')"
COPY tentaflow-containers/tts/docker/kokoro/server.py /app/server.py

COPY tentaflow-containers/tts/docker/kokoro/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

HEALTHCHECK --interval=30s --timeout=5s --start-period=120s --retries=3 \
    CMD python -c "import urllib.request,os; urllib.request.urlopen('http://127.0.0.1:'+os.environ.get('PORT','8880')+'/healthz').read()" || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
