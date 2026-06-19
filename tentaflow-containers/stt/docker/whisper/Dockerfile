# =============================================================================
# Plik: Dockerfile
# Opis: Kontener STT z whisper.cpp (OpenAI-compatible /v1/audio/transcriptions),
#       direct-http (bez sidecara). CUDA build dla maksymalnej predkosci.
# =============================================================================

# CUDA 13.0 (NIE 13.1 - regresja/crash MMQ w llama.cpp/GGML).
FROM nvidia/cuda:13.0.3-devel-ubuntu24.04 AS whisper-builder
RUN apt-get update && apt-get install -y --no-install-recommends \
      git cmake build-essential ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /build
RUN git clone --depth 1 https://github.com/ggml-org/whisper.cpp.git
WORKDIR /build/whisper.cpp
# Jawna lista architektur GPU - GGML nie ma sm_103 w domyslnej liscie.
# 103-real = B300, 100-real = B200; pozostale (Turing/Ampere/Ada/Hopper/Blackwell
# consumer) dla kompatybilnosci wstecz z innymi GPU.
RUN cmake -B build -DGGML_CUDA=ON -DWHISPER_SERVER=ON -DCMAKE_BUILD_TYPE=Release \
      -DCMAKE_CUDA_ARCHITECTURES="75-real;80-real;86-real;89-real;90-real;100-real;103-real;120-real" \
    && cmake --build build --target whisper-server -j"$(nproc)" \
    && strip build/bin/whisper-server

FROM nvidia/cuda:13.0.3-runtime-ubuntu24.04
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
COPY --from=whisper-builder /build/whisper.cpp/build/bin/whisper-server /usr/local/bin/whisper-server
COPY tentaflow-containers/stt/docker/whisper/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

HEALTHCHECK --interval=30s --timeout=5s --start-period=60s --retries=3 \
    CMD curl -fsS "http://127.0.0.1:${PORT:-8081}/health" || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
