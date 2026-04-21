# syntax=docker/dockerfile:1.4
# =============================================================================
# Plik: Dockerfile
# Opis: Obraz kontenera meeting sidecar — Chromium + PulseAudio + Xvfb
#       do automatycznego dolaczania do spotkan Teams/Meet/Zoom.
#
# Wymagania buildera:
#   - Docker Buildx + BuildKit (dla --mount=type=cache)
#   - Uzywane jest `cargo fetch` -> cache registry + prebuild dependencies
#     w osobnej warstwie, zeby edit jednego pliku .rs dawal ~20s rebuild
#     zamiast peach 3 minut.
# =============================================================================

# ---------------------------------------------------------------------------
# Etap 1: kompilacja binary
# ---------------------------------------------------------------------------
FROM rust:1.94-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config \
        libasound2-dev \
        libpulse-dev \
        libssl-dev \
        protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Kopiujemy kod zrodlowy 3 crate'ow (protocol + voice + teams-bot).
# Workspace z lib dependencies nie pozwala sensownie pre-kompilowac zaleznosci
# w osobnej warstwie Docker (stub lib.rs musialby replikowac public API),
# wiec zamiast tego polegamy wylacznie na BuildKit cache mount dla
# /usr/local/cargo/{registry,git} i /build/target.
#
# Dzieki temu:
#   - pierwszy cold build: ~3-5 min (normalnie)
#   - kolejne buildy po edit pliku .rs: ~20-40s bo /build/target zachowuje
#     wszystkie skompilowane deps w BuildKit local cache
#   - zmiany w Cargo.toml: ~1-2 min (cargo rebudowuje tylko zmienione deps)
#
# Binary kopiujemy POZA /build/target (cache mount znika po RUN) do /artifacts.
COPY tentaflow-protocol/ tentaflow-protocol/
COPY tentaflow-voice/ tentaflow-voice/
COPY tentaflow-containers/agents/docker/teams-bot/ tentaflow-containers/teams-bot/

RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/build/target,sharing=locked \
    cargo build --release --manifest-path tentaflow-containers/teams-bot/Cargo.toml \
    && mkdir -p /artifacts \
    && cp /build/tentaflow-containers/teams-bot/target/release/tentaflow-meeting /artifacts/tentaflow-meeting \
    && strip /artifacts/tentaflow-meeting

# ---------------------------------------------------------------------------
# Etap 2: obraz runtime
# ---------------------------------------------------------------------------
FROM debian:bookworm-slim

# Unikamy interaktywnych promptow apt
ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update && apt-get install -y --no-install-recommends \
        chromium \
        pulseaudio \
        libpulse0 \
        xvfb \
        ca-certificates \
        dbus \
        x11vnc \
        novnc \
        websockify \
    && rm -rf /var/lib/apt/lists/*

# Silero VAD uruchamiany przez pure Rust (tentaflow-voice) — brak onnxruntime.so

# Uzytkownik bez uprawnien root
RUN useradd -m -s /bin/bash sidecar

# Konfiguracja PulseAudio
COPY tentaflow-containers/agents/docker/teams-bot/pulseaudio.conf /etc/pulse/default.pa

# Model Silero VAD (diarization jest w routerze, nie w bocie)
RUN mkdir -p /opt/models
COPY tentaflow-containers/agents/docker/teams-bot/models/silero_vad.onnx /opt/models/silero_vad.onnx

# Binary z etapu budowania (skopiowane poza cache mount do /artifacts)
COPY --from=builder /artifacts/tentaflow-meeting /usr/local/bin/tentaflow-meeting

# Port QUIC (UDP)
EXPOSE 5000/udp
# VNC bezposredni
EXPOSE 5900
# noVNC interfejs webowy
EXPOSE 6080

# Katalog roboczy
WORKDIR /home/sidecar
USER sidecar

# DISPLAY dla Xvfb — Chromium uzywa tego do renderowania na wirtualnym ekranie
ENV DISPLAY=:99
# XDG_RUNTIME_DIR — PulseAudio i Chromium szukaja socketow tutaj
ENV XDG_RUNTIME_DIR=/tmp/runtime

# Skrypt startowy — PulseAudio (minimal) + Xvfb + VNC + sidecar
# Audio przechwytywane w Chromium przez JS (browser_inject.js), nie przez PulseAudio.
ENTRYPOINT ["/bin/bash", "-c", "\
    rm -f /tmp/.X99-lock /tmp/.X11-unix/X99 2>/dev/null; \
    mkdir -p /tmp/runtime && \
    pulseaudio --start --exit-idle-time=-1 --log-level=error 2>/dev/null; \
    Xvfb :99 -screen 0 1920x1080x24 -ac & \
    sleep 1 && \
    x11vnc -display :99 -forever -nopw -shared -rfbport 5900 & \
    websockify --web /usr/share/novnc 6080 localhost:5900 & \
    exec /usr/local/bin/tentaflow-meeting \"$@\""]
