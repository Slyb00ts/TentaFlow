# =============================================================================
# Plik: Dockerfile
# Opis: Kontener TTS sherpa-onnx (modele VITS Piper) — direct-http (bez
#       sidecara). Python FastAPI (server.py) wystawia /audio/speech; model VITS
#       pobierany w runtime z repo HF (env MODEL). Upstream sherpa-onnx nie ma
#       gotowego binarnego serwera HTTP TTS, wiec uzywamy pip-owego wrappera.
# =============================================================================

FROM python:3.11-slim

ENV PIP_NO_CACHE_DIR=1 \
    PYTHONUNBUFFERED=1 \
    HF_HOME=/app/cache/huggingface

RUN apt-get update && apt-get install -y --no-install-recommends \
        libsndfile1 ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY tentaflow-containers/tts/docker/sherpa-onnx/requirements.txt .
RUN pip install --upgrade pip && pip install -r requirements.txt

COPY tentaflow-containers/tts/docker/sherpa-onnx/server.py /app/server.py
COPY tentaflow-containers/tts/docker/sherpa-onnx/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

HEALTHCHECK --interval=30s --timeout=5s --start-period=120s --retries=3 \
    CMD python -c "import urllib.request,os; urllib.request.urlopen('http://127.0.0.1:'+os.environ.get('PORT','8084')+'/healthz').read()" || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
