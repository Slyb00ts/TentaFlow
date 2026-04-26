# =============================================================================
# Plik: Dockerfile (Kokoro 82M TTS via kokoro-onnx)
# Opis: Lekki obraz Linux/Windows-Docker uruchamiajacy Kokoro 82M przez
#       ONNX Runtime + FastAPI (`/v1/audio/speech` OpenAI-compatible).
#       Wagi i voices pobierane przy pierwszym starcie z onnx-community/Kokoro-82M-v1.0-ONNX.
# =============================================================================

FROM python:3.11-slim

ENV PIP_NO_CACHE_DIR=1 \
    PIP_DISABLE_PIP_VERSION_CHECK=1 \
    PYTHONUNBUFFERED=1 \
    KOKORO_PORT=8880 \
    HF_HOME=/app/cache/huggingface

RUN apt-get update && apt-get install -y --no-install-recommends \
        libsndfile1 \
        espeak-ng \
        ffmpeg \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY requirements.txt .
RUN pip install --upgrade pip && pip install -r requirements.txt

# Pre-download model + voices przy buildzie zeby pierwsze uruchomienie nie
# bylo ~30 s wolniejsze. ~340 MB total.
RUN python -c "import os; os.makedirs('/app/models', exist_ok=True); \
from huggingface_hub import hf_hub_download; \
hf_hub_download('onnx-community/Kokoro-82M-v1.0-ONNX', 'onnx/model.onnx', local_dir='/app/models'); \
hf_hub_download('onnx-community/Kokoro-82M-v1.0-ONNX', 'voices/af_heart.bin', local_dir='/app/models'); \
hf_hub_download('onnx-community/Kokoro-82M-v1.0-ONNX', 'voices/af_bella.bin', local_dir='/app/models'); \
hf_hub_download('onnx-community/Kokoro-82M-v1.0-ONNX', 'voices/am_michael.bin', local_dir='/app/models'); \
hf_hub_download('onnx-community/Kokoro-82M-v1.0-ONNX', 'voices/am_adam.bin', local_dir='/app/models'); \
hf_hub_download('onnx-community/Kokoro-82M-v1.0-ONNX', 'voices/bf_alice.bin', local_dir='/app/models'); \
hf_hub_download('onnx-community/Kokoro-82M-v1.0-ONNX', 'voices/bm_george.bin', local_dir='/app/models')"

COPY server.py /app/

EXPOSE 8880

CMD ["sh", "-c", "uvicorn server:app --host 0.0.0.0 --port ${KOKORO_PORT:-8880}"]
