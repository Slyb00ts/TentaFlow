# =============================================================================
# Plik: Dockerfile
# Opis: Kontener TTS z sherpa-onnx (modele jarvis, justyna) + sidecar.
#       sherpa-onnx-offline-tts-server eksponuje /tts (custom JSON), sidecar
#       forwarduje surowo (api = "raw_http") i nie tlumaczy do OpenAI.
# =============================================================================

FROM rust:slim-bookworm AS sidecar-builder
WORKDIR /build
RUN apt-get update && apt-get install -y --no-install-recommends \
      pkg-config libssl-dev cmake build-essential \
    && rm -rf /var/lib/apt/lists/*
COPY tentaflow-protocol /build/tentaflow-protocol
COPY tentaflow-containers/sidecar /build/tentaflow-containers/sidecar
WORKDIR /build/tentaflow-containers/sidecar
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/tentaflow-containers/sidecar/target \
    cargo build --release --bin tentaflow-sidecar \
    && cp target/release/tentaflow-sidecar /tentaflow-sidecar

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
COPY --from=sidecar-builder /tentaflow-sidecar /usr/local/bin/tentaflow-sidecar
COPY tentaflow-containers/tts-sherpa/config.default.toml /app/config.default.toml
COPY tentaflow-containers/tts-sherpa/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

EXPOSE 5000/udp

HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
    CMD pgrep tentaflow-sidecar || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
