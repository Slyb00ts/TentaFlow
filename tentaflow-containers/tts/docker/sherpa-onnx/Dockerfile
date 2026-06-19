# =============================================================================
# Plik: Dockerfile
# Opis: Kontener TTS z sherpa-onnx (modele jarvis, justyna) — direct-http.
#       sherpa-onnx-offline-tts-server eksponuje /tts (custom JSON); Core gada
#       HTTP wprost do host-mapped portu (no sidecar).
# =============================================================================

FROM debian:bookworm-slim AS sherpa-builder
RUN apt-get update && apt-get install -y --no-install-recommends \
      git cmake build-essential ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /build
RUN git clone --depth 1 https://github.com/k2-fsa/sherpa-onnx.git
WORKDIR /build/sherpa-onnx
RUN cmake -B build -DSHERPA_ONNX_ENABLE_TTS=ON -DBUILD_SHARED_LIBS=OFF \
      -DCMAKE_BUILD_TYPE=Release \
    && cmake --build build --target sherpa-onnx-offline-tts-server -j"$(nproc)" \
    && strip build/bin/sherpa-onnx-offline-tts-server

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl libgomp1 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=sherpa-builder /build/sherpa-onnx/build/bin/sherpa-onnx-offline-tts-server /usr/local/bin/sherpa-tts
COPY tentaflow-containers/tts/docker/sherpa-onnx/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
    CMD pgrep -f sherpa-tts || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
