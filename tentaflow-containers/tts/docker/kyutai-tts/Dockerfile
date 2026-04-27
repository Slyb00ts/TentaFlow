# =============================================================================
# Plik: Dockerfile (Kyutai Pocket TTS — maly model 100M, CPU)
# Opis: Lekki obraz Linux/Win/Mac. ~200 MB model, ~6x realtime na M-series CPU.
# =============================================================================

FROM python:3.12-slim

ENV PIP_NO_CACHE_DIR=1 \
    PYTHONUNBUFFERED=1 \
    POCKET_TTS_PORT=8088 \
    POCKET_TTS_LANGUAGE=english \
    HF_HOME=/app/cache/huggingface

RUN apt-get update && apt-get install -y --no-install-recommends \
        libsndfile1 \
        ffmpeg \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY requirements.txt .
RUN pip install --upgrade pip && pip install -r requirements.txt

# Pre-download wagi pocket-tts przy buildzie. ~200 MB.
RUN python -c "from pocket_tts import TTSModel; TTSModel.load_model()"

COPY server.py /app/

EXPOSE 8088

CMD ["sh", "-c", "uvicorn server:app --host 0.0.0.0 --port ${POCKET_TTS_PORT:-8088}"]
