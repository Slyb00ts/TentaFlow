#!/bin/bash
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$SCRIPT_DIR/../.."
CORE_DIR="$PROJECT_ROOT/core"

echo "=== Building TentaFlow Mobile (Rust core) ==="

# Targets
DEVICE_TARGET="aarch64-apple-ios"
SIMULATOR_TARGET="aarch64-apple-ios-sim"

# Minimum iOS version — musi pasowac do Info.plist i Xcode project
IOS_MIN_VERSION="16.0"

# Ensure targets are installed
rustup target add "$DEVICE_TARGET" 2>/dev/null || true
rustup target add "$SIMULATOR_TARGET" 2>/dev/null || true

BUILD_MODE="${1:-release}"
CARGO_FLAGS=""
OUTPUT_DIR="debug"

if [ "$BUILD_MODE" = "release" ]; then
    CARGO_FLAGS="--release"
    OUTPUT_DIR="release"
fi

# Flagi kompilatora C/C++ — minimum iOS version + stack probe fix
export IPHONEOS_DEPLOYMENT_TARGET="$IOS_MIN_VERSION"
export CFLAGS="-mios-version-min=$IOS_MIN_VERSION"
export CXXFLAGS="-mios-version-min=$IOS_MIN_VERSION"

# ___chkstk_darwin fix — ta funkcja nie istnieje na iOS,
# ale jest generowana przez kompilator dla duzych ramek stosu.
# Linkujemy z libclang_rt ktora ja dostarcza.
SDKROOT=$(xcrun --sdk iphoneos --show-sdk-path)
CLANG_RT_DIR=$(dirname $(xcrun --toolchain default -f clang))/../lib/clang
CLANG_VERSION=$(ls "$CLANG_RT_DIR" | sort -V | tail -1)
CLANG_RT_LIB="$CLANG_RT_DIR/$CLANG_VERSION/lib/darwin/libclang_rt.ios.a"

if [ -f "$CLANG_RT_LIB" ]; then
    echo "Using clang_rt: $CLANG_RT_LIB"
    export RUSTFLAGS="-C link-arg=$CLANG_RT_LIB -C link-arg=-mios-version-min=$IOS_MIN_VERSION"
else
    echo "WARNING: clang_rt.ios.a nie znalezione, szukam alternatywy..."
    # Alternatywna sciezka dla nowszych Xcode
    CLANG_RT_LIB2=$(find "$(xcode-select -p)" -name "libclang_rt.ios.a" 2>/dev/null | head -1)
    if [ -n "$CLANG_RT_LIB2" ]; then
        echo "Using clang_rt: $CLANG_RT_LIB2"
        export RUSTFLAGS="-C link-arg=$CLANG_RT_LIB2 -C link-arg=-mios-version-min=$IOS_MIN_VERSION"
    else
        echo "WARNING: Brak libclang_rt.ios.a — ___chkstk_darwin moze byc undefined"
        export RUSTFLAGS="-C link-arg=-mios-version-min=$IOS_MIN_VERSION"
    fi
fi

echo "IPHONEOS_DEPLOYMENT_TARGET=$IOS_MIN_VERSION"
echo "RUSTFLAGS=$RUSTFLAGS"

echo ""
echo "Building for device ($DEVICE_TARGET)..."
cd "$CORE_DIR"
cargo build --target "$DEVICE_TARGET" $CARGO_FLAGS || { echo "ERROR: Build dla device FAILED!"; exit 1; }

echo ""
echo "Building for simulator ($SIMULATOR_TARGET)..."
echo "UWAGA: Simulator build moze sie nie powiesc (MLX wymaga Metal na fizycznym urzadzeniu)"
cargo build --target "$SIMULATOR_TARGET" $CARGO_FLAGS || echo "Simulator build pominity (oczekiwane — MLX nie obsluguje symulatora)"

# Output paths — target dir jest w katalogu Mobile (workspace member)
DEVICE_LIB="$PROJECT_ROOT/target/$DEVICE_TARGET/$OUTPUT_DIR/libtentaflow_mobile.a"
SIMULATOR_LIB="$PROJECT_ROOT/target/$SIMULATOR_TARGET/$OUTPUT_DIR/libtentaflow_mobile.a"
OUTPUT_FAT="$SCRIPT_DIR/../libtentaflow_mobile.a"

echo ""
echo "Device library: $DEVICE_LIB"
echo "Simulator library: $SIMULATOR_LIB"

if [ -f "$DEVICE_LIB" ]; then
    cp "$DEVICE_LIB" "$OUTPUT_FAT"
    echo "Copied device library to $OUTPUT_FAT"
    echo "Size: $(du -h "$OUTPUT_FAT" | cut -f1)"
else
    echo "ERROR: Brak pliku $DEVICE_LIB — build prawdopodobnie FAILED!"
    exit 1
fi

echo ""
echo "=== Build complete ==="
echo ""
echo "Nastepne kroki:"
echo "  1. Otworz ios/TentaFlowAI.xcodeproj w Xcode"
echo "  2. Ustaw Development Team (Signing & Capabilities)"
echo "  3. Podlacz iPhone i wybierz jako target"
echo "  4. Cmd+R (Build & Run)"
