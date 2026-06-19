# =============================================================================
# Plik: Dockerfile
# Opis: Kontener TTS z XTTS v2 (coqui) — voice cloning. Direct-http (no sidecar).
# =============================================================================

FROM nvidia/cuda:13.0.3-runtime-ubuntu24.04
RUN apt-get update && apt-get install -y --no-install-recommends \
      python3 python3-pip python3-venv python3-dev git ffmpeg curl ca-certificates \
      libsndfile1 gcc g++ \
    && rm -rf /var/lib/apt/lists/*

RUN python3 -m venv /opt/venv
ENV PATH="/opt/venv/bin:$PATH"
# torch cu130 jawnie PRZED coqui-tts (cu130 dla B300/sm_103). Spojne z native
# bundle: coqui-tts==0.27.4 (fork IDIAP), torchcodec (od torch 2.9 wymagany do
# audio IO), force-pin transformers/tokenizers/hub (coqui XTTS gpt.py uzywa
# isin_mps_friendly usunietego w transformers>=4.57).
RUN pip install --no-cache-dir --upgrade pip wheel \
 && pip install --no-cache-dir \
      torch==2.11.0 torchaudio==2.11.0 \
      --index-url https://download.pytorch.org/whl/cu130 \
 && pip install --no-cache-dir torchcodec \
 && pip install --no-cache-dir \
      "coqui-tts==0.27.4" \
      fastapi uvicorn \
 && pip install --no-cache-dir \
      "transformers==4.55.4" "tokenizers==0.21.4" "huggingface-hub==0.34.4"

COPY tentaflow-containers/tts/docker/xtts/server.py /app/server.py
COPY tentaflow-containers/tts/docker/xtts/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

HEALTHCHECK --interval=30s --timeout=5s --start-period=120s --retries=3 \
    CMD pgrep -f uvicorn || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
