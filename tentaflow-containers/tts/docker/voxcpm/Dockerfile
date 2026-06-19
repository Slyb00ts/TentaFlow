# =============================================================================
# Plik: Dockerfile
# Opis: Kontener TTS z VoxCPM2 — direct-http (no sidecar).
# =============================================================================

FROM nvidia/cuda:13.0.3-runtime-ubuntu24.04
RUN apt-get update && apt-get install -y --no-install-recommends \
      python3 python3-pip python3-venv python3-dev git ffmpeg curl ca-certificates \
      libsndfile1 gcc g++ \
    && rm -rf /var/lib/apt/lists/*

RUN python3 -m venv /opt/venv
ENV PATH="/opt/venv/bin:$PATH"
# torch cu130 jawnie PRZED voxcpm (B300/sm_103 wymaga CUDA 13). Spojne z native
# bundle: voxcpm instalowany jako pakiet z git (nie clone+PYTHONPATH), ciagnie
# wlasne deps (transformers itd.); soundfile/fastapi/uvicorn dla serwera.
RUN pip install --no-cache-dir --upgrade pip wheel \
 && pip install --no-cache-dir \
      torch==2.11.0 torchaudio==2.11.0 \
      --index-url https://download.pytorch.org/whl/cu130 \
 && pip install --no-cache-dir \
      "git+https://github.com/OpenBMB/VoxCPM.git" \
      soundfile fastapi uvicorn

COPY tentaflow-containers/tts/docker/voxcpm/server.py /app/server.py
COPY tentaflow-containers/tts/docker/voxcpm/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

HEALTHCHECK --interval=30s --timeout=5s --start-period=120s --retries=3 \
    CMD pgrep -f uvicorn || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
