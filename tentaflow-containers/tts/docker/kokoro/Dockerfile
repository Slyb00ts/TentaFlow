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
# Model i glosy z tego samego wydania kokoro-onnx — model.onnx z onnx-community
# ma niezgodne typy wejsc ONNX (int32 vs float). voices-v1.0.bin to npz z 54
# glosami ladowany przez np.load.
RUN python -c "import os,urllib.request; os.makedirs('/app/models', exist_ok=True); \
base='https://github.com/thewh1teagle/kokoro-onnx/releases/download/model-files-v1.0/'; \
urllib.request.urlretrieve(base+'kokoro-v1.0.onnx', '/app/models/kokoro-v1.0.onnx'); \
urllib.request.urlretrieve(base+'voices-v1.0.bin', '/app/models/voices-v1.0.bin')"
COPY tentaflow-containers/tts/docker/kokoro/server.py /app/server.py

COPY tentaflow-containers/tts/docker/kokoro/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

HEALTHCHECK --interval=30s --timeout=5s --start-period=120s --retries=3 \
    CMD python -c "import urllib.request,os; urllib.request.urlopen('http://127.0.0.1:'+os.environ.get('PORT','8880')+'/healthz').read()" || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
