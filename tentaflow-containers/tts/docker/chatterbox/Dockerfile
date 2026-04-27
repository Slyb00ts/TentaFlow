# =============================================================================
# Plik: Dockerfile (Chatterbox Multilingual TTS)
# Opis: Linux/Windows-Docker uruchomieniowy. macOS uzywa python-bundle (MPS).
# =============================================================================

FROM python:3.11-slim

ENV PIP_NO_CACHE_DIR=1 \
    PYTHONUNBUFFERED=1 \
    CHATTERBOX_PORT=8087 \
    HF_HOME=/app/cache/huggingface \
    CHATTERBOX_DEVICE=cpu

RUN apt-get update && apt-get install -y --no-install-recommends \
        libsndfile1 \
        ffmpeg \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY requirements.txt .
RUN pip install --upgrade pip && pip install -r requirements.txt

# Pre-download wagi modelu (Multilingual ~1.5 GB na dysk).
RUN python -c "from chatterbox.mtl_tts import ChatterboxMultilingualTTS; \
ChatterboxMultilingualTTS.from_pretrained(device='cpu')"

COPY server.py /app/

EXPOSE 8087

CMD ["sh", "-c", "uvicorn server:app --host 0.0.0.0 --port ${CHATTERBOX_PORT:-8087}"]
