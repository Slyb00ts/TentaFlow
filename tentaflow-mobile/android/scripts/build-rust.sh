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
