# =============================================================================
# Plik: Dockerfile (Kyutai TTS via moshi)
# Opis: Streaming TTS Kyutai 1.8B (model.id `kyutai/tts-1.6b-en_fr`).
#       Oparte o `moshi` Python package + Mimi codec. CPU/CUDA. EN+FR.
#       Voices z `kyutai/tts-voices` repo.
# =============================================================================

FROM python:3.12-slim

ENV PIP_NO_CACHE_DIR=1 \
    PYTHONUNBUFFERED=1 \
    KYUTAI_PORT=8881 \
    HF_HOME=/app/cache/huggingface \
    TORCH_HOME=/app/cache/torch \
    KYUTAI_DEVICE=cpu

RUN apt-get update && apt-get install -y --no-install-recommends \
        libsndfile1 \
        ffmpeg \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY requirements.txt .
RUN pip install --upgrade pip && pip install -r requirements.txt

# Pre-download wagi modelu ~3.6 GB + 8 default voices przy buildzie (kompromis:
# wieksze warstwy obrazu, ale predszy pierwszy run).
RUN python -c "from huggingface_hub import snapshot_download; \
snapshot_download('kyutai/tts-1.6b-en_fr', allow_patterns=['*.safetensors','*.json','*.txt']); \
snapshot_download('kyutai/tts-voices', allow_patterns=['expresso/*.safetensors'])"

COPY server.py /app/

EXPOSE 8881

CMD ["sh", "-c", "uvicorn server:app --host 0.0.0.0 --port ${KYUTAI_PORT:-8881}"]
