# =============================================================================
# Plik: Dockerfile
# Opis: Obraz kontenera meeting sidecar — Chromium + PulseAudio + Xvfb
#       do automatycznego dolaczania do spotkan Teams/Meet/Zoom.
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
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Kopiujemy manifesty crate'ow osobno — cache warstw cargo fetch
COPY tentaflow-protocol/Cargo.toml tentaflow-protocol/Cargo.toml
COPY tentaflow-containers/teams-bot/Cargo.toml tentaflow-containers/teams-bot/Cargo.toml

# Tymczasowe puste src zeby cargo fetch zadzialalal
RUN mkdir -p tentaflow-protocol/src && echo "" > tentaflow-protocol/src/lib.rs \
    && mkdir -p tentaflow-containers/teams-bot/src && echo "fn main() {}" > tentaflow-containers/teams-bot/src/main.rs

RUN cargo fetch --manifest-path tentaflow-containers/teams-bot/Cargo.toml

# Kopiujemy wlasciwy kod zrodlowy
COPY tentaflow-protocol/ tentaflow-protocol/
COPY tentaflow-containers/teams-bot/ tentaflow-containers/teams-bot/

# Kompilacja release
RUN cargo build --release --manifest-path tentaflow-containers/teams-bot/Cargo.toml \
    && strip /build/tentaflow-containers/teams-bot/target/release/tentaflow-meeting

# ---------------------------------------------------------------------------
# Etap 2: obraz runtime
# ---------------------------------------------------------------------------
FROM debian:bookworm-slim

# Unikamy interaktywnych promptow apt
ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update && apt-get install -y --no-install-recommends \
        chromium \
        pulseaudio \
        pulseaudio-utils \
        libpulse0 \
        xvfb \
        ca-certificates \
        dbus \
        x11vnc \
        novnc \
        websockify \
    && rm -rf /var/lib/apt/lists/*

# Uzytkownik bez uprawnien root
RUN useradd -m -s /bin/bash sidecar

# Konfiguracja PulseAudio
COPY tentaflow-containers/teams-bot/pulseaudio.conf /etc/pulse/default.pa

# Binary z etapu budowania
COPY --from=builder /build/tentaflow-containers/teams-bot/target/release/tentaflow-meeting /usr/local/bin/tentaflow-meeting

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

# Skrypt startowy — PulseAudio + Xvfb + sidecar
ENTRYPOINT ["/bin/bash", "-c", "\
    rm -f /tmp/.X99-lock /tmp/.X11-unix/X99 2>/dev/null; \
    mkdir -p /tmp/runtime && \
    pulseaudio --start --exit-idle-time=-1 --log-level=error 2>/dev/null; \
    Xvfb :99 -screen 0 1920x1080x24 -ac &\
    sleep 1 && \
    x11vnc -display :99 -forever -nopw -shared -rfbport 5900 & \
    websockify --web /usr/share/novnc 6080 localhost:5900 & \
    exec /usr/local/bin/tentaflow-meeting \"$@\""]
