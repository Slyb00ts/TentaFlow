# =============================================================================
# Plik: Dockerfile
# Opis: kyutai-tts TTS server — direct-http (no sidecar). uvicorn server.py
#       nasluchuje na 0.0.0.0:${PORT}.
# =============================================================================

FROM python:3.12-slim

ENV PIP_NO_CACHE_DIR=1 \
    PYTHONUNBUFFERED=1 \
    HF_HOME=/app/cache/huggingface

RUN apt-get update && apt-get install -y --no-install-recommends \
        libsndfile1 ffmpeg ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY tentaflow-containers/tts/docker/kyutai-tts/requirements.txt .
RUN pip install --upgrade pip && pip install -r requirements.txt
RUN python -c "from pocket_tts import TTSModel; TTSModel.load_model()"
COPY tentaflow-containers/tts/docker/kyutai-tts/server.py /app/server.py

COPY tentaflow-containers/tts/docker/kyutai-tts/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

HEALTHCHECK --interval=30s --timeout=5s --start-period=120s --retries=3 \
    CMD python -c "import urllib.request,os; urllib.request.urlopen('http://127.0.0.1:'+os.environ.get('PORT','8088')+'/healthz').read()" || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
