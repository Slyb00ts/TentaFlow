# =============================================================================
# Plik: Dockerfile
# Opis: Kontener STT z NVIDIA NeMo Parakeet-TDT-0.6B-v3, direct-http (bez
#       sidecara). Eksponuje OpenAI-compatible /v1/audio/transcriptions przez
#       maly FastAPI wrapper uruchamiany w tym samym venv.
# =============================================================================

FROM nvidia/cuda:13.0.3-devel-ubuntu24.04
RUN apt-get update && apt-get install -y --no-install-recommends \
      python3 python3-pip python3-venv git ffmpeg curl ca-certificates \
      build-essential libsndfile1 \
    && rm -rf /var/lib/apt/lists/*

RUN python3 -m venv /opt/venv
ENV PATH="/opt/venv/bin:$PATH"
# torch cu130 instalowany jawnie PRZED NeMo, bo NeMo ciagnie torch posrednio
# (NGC <=25.03 z torch cu128 crashuje na B300/sm_103 — wymuszamy cu130)
RUN pip install --no-cache-dir --upgrade pip wheel \
 && pip install --no-cache-dir \
      torch>=2.11 torchaudio \
      --index-url https://download.pytorch.org/whl/cu130 \
 && pip install --no-cache-dir \
      "nemo_toolkit[asr]" \
      fastapi uvicorn python-multipart soundfile

COPY tentaflow-containers/stt/docker/parakeet/server.py /app/server.py
COPY tentaflow-containers/stt/docker/parakeet/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

HEALTHCHECK --interval=30s --timeout=5s --start-period=120s --retries=3 \
    CMD curl -fsS "http://127.0.0.1:${PORT:-8082}/health" || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
