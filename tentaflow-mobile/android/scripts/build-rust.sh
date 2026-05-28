#!/bin/bash
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$SCRIPT_DIR/../.."
CORE_DIR="$PROJECT_ROOT/core"
JNILIBS_DIR="$SCRIPT_DIR/../app/src/main/jniLibs"

echo "=== Building TentaFlow Mobile (Rust core for Android) ==="

# Ensure cargo-ndk is installed
if ! command -v cargo-ndk &> /dev/null; then
    echo "Installing cargo-ndk..."
    cargo install cargo-ndk
fi

# Ensure Android targets are installed
rustup target add aarch64-linux-android 2>/dev/null || true
rustup target add armv7-linux-androideabi 2>/dev/null || true
rustup target add x86_64-linux-android 2>/dev/null || true

find_gstreamer_android_pkg_config_dir() {
    local target_hint="$1"
    local allow_fallback="${2:-false}"
    local root="${GSTREAMER_ANDROID_ROOT:-}"
    if [ -z "$root" ]; then
        root="$HOME/Library/GStreamer"
    fi
    if [ ! -d "$root" ]; then
        return 1
    fi

    local match
    match=$(find "$root" -path "*/lib/pkgconfig/gstreamer-1.0.pc" 2>/dev/null | grep -E "$target_hint" | head -1 || true)
    if [ -z "$match" ]; then
        match=$(find "$root" -path "*/pkgconfig/gstreamer-1.0.pc" 2>/dev/null | grep -E "$target_hint" | head -1 || true)
    fi
    if [ -z "$match" ] && [ "$allow_fallback" = "true" ]; then
        match=$(find "$root" -path "*/pkgconfig/gstreamer-1.0.pc" 2>/dev/null | head -1 || true)
    fi
    if [ -n "$match" ]; then
        dirname "$match"
        return 0
    fi

    return 1
}

configure_gstreamer_android() {
    local arm64 armv7 x64
    if ! arm64="$(find_gstreamer_android_pkg_config_dir 'arm64|aarch64')"; then
        echo "ERROR: Nie znaleziono GStreamer Android SDK."
        echo "Zainstaluj oficjalny GStreamer Android SDK i ustaw GSTREAMER_ANDROID_ROOT."
        echo "Oczekiwany plik dla arm64/aarch64: */pkgconfig/gstreamer-1.0.pc"
        exit 1
    fi
    armv7="$(find_gstreamer_android_pkg_config_dir 'armv7|armeabi|androideabi' true)" || armv7="$arm64"
    x64="$(find_gstreamer_android_pkg_config_dir 'x86_64' true)" || x64="$arm64"

    export PKG_CONFIG_ALLOW_CROSS=1
    export PKG_CONFIG_PATH_aarch64_linux_android="$arm64"
    export PKG_CONFIG_PATH_armv7_linux_androideabi="$armv7"
    export PKG_CONFIG_PATH_x86_64_linux_android="$x64"
    echo "GStreamer Android pkg-config arm64-v8a: $arm64"
    echo "GStreamer Android pkg-config armeabi-v7a: $armv7"
    echo "GStreamer Android pkg-config x86_64: $x64"
}

configure_gstreamer_android

BUILD_MODE="${1:-release}"
CARGO_FLAGS=""

if [ "$BUILD_MODE" = "release" ]; then
    CARGO_FLAGS="--release"
fi

echo "Building for Android targets..."
cd "$CORE_DIR"

cargo ndk \
    -t arm64-v8a \
    -t armeabi-v7a \
    -t x86_64 \
    -o "$JNILIBS_DIR" \
    build $CARGO_FLAGS

echo ""
echo "=== Build complete ==="
echo "JNI libraries placed in: $JNILIBS_DIR"
ls -la "$JNILIBS_DIR"/*/libtentaflow_mobile.so 2>/dev/null || echo "No .so files found yet"
