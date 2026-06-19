# =============================================================================
# Plik: Dockerfile
# Opis: Kontener STT z Qwen3-ASR-1.7B, direct-http (bez sidecara).
#       Uzywa transformers + flash-attn (CUDA) z FastAPI wrapperem
#       (vLLM nie ma jeszcze formalnego supportu Qwen-ASR multimodal).
# =============================================================================

FROM nvidia/cuda:13.0.3-devel-ubuntu24.04
RUN apt-get update && apt-get install -y --no-install-recommends \
      python3 python3-pip python3-venv git ffmpeg curl ca-certificates \
      build-essential libsndfile1 \
    && rm -rf /var/lib/apt/lists/*

RUN python3 -m venv /opt/venv
ENV PATH="/opt/venv/bin:$PATH"
# torch cu130 jawnie z dedykowanego indexu (B300/sm_103 wymaga CUDA 13)
RUN pip install --no-cache-dir --upgrade pip wheel \
 && pip install --no-cache-dir \
      torch>=2.11 torchaudio \
      --index-url https://download.pytorch.org/whl/cu130 \
 && pip install --no-cache-dir \
      "transformers>=4.45" accelerate \
      fastapi uvicorn python-multipart soundfile librosa
# flash-attn buduje sie dlugo; jesli odpadnie kontener uzyje SDPA backend
RUN pip install --no-cache-dir flash-attn --no-build-isolation || \
    echo "[warn] flash-attn build failed — uzywam SDPA"

COPY tentaflow-containers/stt/docker/qwen-asr/server.py /app/server.py
COPY tentaflow-containers/stt/docker/qwen-asr/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

HEALTHCHECK --interval=30s --timeout=5s --start-period=180s --retries=3 \
    CMD curl -fsS "http://127.0.0.1:${PORT:-8083}/health" || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
