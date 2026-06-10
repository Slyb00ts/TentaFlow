#!/bin/bash
set -euo pipefail

ANDROID_SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MOBILE_ROOT="$ANDROID_SCRIPT_DIR/../.."
REPO_ROOT="$(cd "$MOBILE_ROOT/.." && pwd)"
source "$REPO_ROOT/scripts/native-libs/common.sh"

CORE_DIR="$MOBILE_ROOT/core"
JNILIBS_DIR="$ANDROID_SCRIPT_DIR/../app/src/main/jniLibs"
NATIVE_LIBS_DIR="$NATIVE_ROOT"
GSTREAMER_ANDROID_VERSION="${GSTREAMER_ANDROID_VERSION:-1.28.3}"

echo "=== Building TentaFlow Mobile (Rust core for Android) ==="

if ! command -v cargo-ndk &> /dev/null; then
    echo "Installing cargo-ndk..."
    cargo install cargo-ndk
fi

ANDROID_NDK_HOME="$(require_android_ndk)"
ANDROID_HOST_TAG="$(android_host_tag)"
ANDROID_API_LEVEL="${ANDROID_API_LEVEL:-26}"
ANDROID_TOOLCHAIN="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/$ANDROID_HOST_TAG/bin"
export ANDROID_NDK_HOME ANDROID_NDK_ROOT="$ANDROID_NDK_HOME" ANDROID_API_LEVEL

configure_android_ndk_tools() {
    if [ ! -x "$ANDROID_TOOLCHAIN/aarch64-linux-android${ANDROID_API_LEVEL}-clang" ]; then
        echo "ERROR: Nie znaleziono Android NDK clang w $ANDROID_TOOLCHAIN"
        exit 1
    fi

    export CC_aarch64_linux_android="$ANDROID_TOOLCHAIN/aarch64-linux-android${ANDROID_API_LEVEL}-clang"
    export CXX_aarch64_linux_android="$ANDROID_TOOLCHAIN/aarch64-linux-android${ANDROID_API_LEVEL}-clang++"
    export AR_aarch64_linux_android="$ANDROID_TOOLCHAIN/llvm-ar"
    export CC_armv7_linux_androideabi="$ANDROID_TOOLCHAIN/armv7a-linux-androideabi${ANDROID_API_LEVEL}-clang"
    export CXX_armv7_linux_androideabi="$ANDROID_TOOLCHAIN/armv7a-linux-androideabi${ANDROID_API_LEVEL}-clang++"
    export AR_armv7_linux_androideabi="$ANDROID_TOOLCHAIN/llvm-ar"
    export CC_x86_64_linux_android="$ANDROID_TOOLCHAIN/x86_64-linux-android${ANDROID_API_LEVEL}-clang"
    export CXX_x86_64_linux_android="$ANDROID_TOOLCHAIN/x86_64-linux-android${ANDROID_API_LEVEL}-clang++"
    export AR_x86_64_linux_android="$ANDROID_TOOLCHAIN/llvm-ar"
}

platform_for_abi() {
    case "$1" in
        arm64-v8a) printf '%s\n' "android-arm64" ;;
        armeabi-v7a) printf '%s\n' "android-armv7" ;;
        x86_64) printf '%s\n' "android-x86_64" ;;
        *) return 1 ;;
    esac
}

rust_target_for_abi() {
    case "$1" in
        arm64-v8a) printf '%s\n' "aarch64-linux-android" ;;
        armeabi-v7a) printf '%s\n' "armv7-linux-androideabi" ;;
        x86_64) printf '%s\n' "x86_64-linux-android" ;;
        *) return 1 ;;
    esac
}

detect_connected_android_abi() {
    if ! command -v adb >/dev/null 2>&1; then
        return 1
    fi
    if [ "$(adb get-state 2>/dev/null || true)" != "device" ]; then
        return 1
    fi
    adb shell getprop ro.product.cpu.abi 2>/dev/null | tr -d '\r' | head -n1
}

selected_android_abis() {
    local configured="${ANDROID_ABIS:-auto}"
    local detected
    if [ "$configured" = "auto" ]; then
        detected="$(detect_connected_android_abi || true)"
        case "$detected" in
            arm64-v8a|armeabi-v7a|x86_64) printf '%s\n' "$detected"; return 0 ;;
        esac
        printf '%s\n' "arm64-v8a"
        return 0
    fi
    if [ "$configured" = "all" ]; then
        printf '%s\n' "arm64-v8a" "armeabi-v7a" "x86_64"
        return 0
    fi
    printf '%s\n' "$configured" | tr ',' '\n' | tr ' ' '\n' | sed '/^$/d'
}

find_gstreamer_android_pkg_config_dir() {
    local target_hint="$1"
    local allow_fallback="${2:-false}"
    shift 2 || true

    local root match
    for root in "$@"; do
        [ -n "$root" ] && [ -d "$root" ] || continue
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
    done

    return 1
}

install_gstreamer_android_sdk() {
    local install_dir="$NATIVE_CACHE/gstreamer/android/$GSTREAMER_ANDROID_VERSION"
    local marker
    marker="$(find_gstreamer_android_pkg_config_dir 'arm64|aarch64' true "$install_dir" 2>/dev/null || true)"
    if [ -n "$marker" ]; then
        printf '%s\n' "$install_dir"
        return 0
    fi

    require_cmd curl tar sha256sum
    mkdir -p "$install_dir" "$NATIVE_CACHE/downloads"
    local archive="gstreamer-1.0-android-universal-$GSTREAMER_ANDROID_VERSION.tar.xz"
    local base_url="https://gstreamer.freedesktop.org/data/pkg/android/$GSTREAMER_ANDROID_VERSION"
    local archive_path="$NATIVE_CACHE/downloads/$archive"
    local sha_path="$archive_path.sha256sum"

    if [ ! -f "$archive_path" ]; then
        echo "Pobieram GStreamer Android SDK $GSTREAMER_ANDROID_VERSION (~939 MB)..." >&2
        curl -fL --progress-bar -o "$archive_path" "$base_url/$archive"
    fi
    if [ ! -f "$sha_path" ]; then
        curl -fL -o "$sha_path" "$base_url/$archive.sha256sum"
    fi
    ( cd "$(dirname "$archive_path")" && sha256sum -c "$(basename "$sha_path")" ) >&2

    echo "Wypakowuje GStreamer Android SDK do $install_dir..." >&2
    rm -rf "$install_dir"
    mkdir -p "$install_dir"
    tar -xJf "$archive_path" -C "$install_dir"
    printf '%s\n' "$install_dir"
}

configure_gstreamer_android() {
    local arm64 armv7 x64
    local roots=()
    [ -n "${GSTREAMER_ANDROID_ROOT:-}" ] && roots+=("$GSTREAMER_ANDROID_ROOT")
    roots+=("$NATIVE_CACHE/gstreamer/android/$GSTREAMER_ANDROID_VERSION")
    roots+=("$HOME/Library/GStreamer")
    roots+=("/opt/gstreamer-android")

    if ! arm64="$(find_gstreamer_android_pkg_config_dir 'arm64|aarch64' false "${roots[@]}")"; then
        roots=("$(install_gstreamer_android_sdk)")
        if ! arm64="$(find_gstreamer_android_pkg_config_dir 'arm64|aarch64' false "${roots[@]}")"; then
            echo "ERROR: Pobrano GStreamer Android SDK, ale nie znaleziono gstreamer-1.0.pc dla arm64." >&2
            echo "Sprawdz katalog: ${roots[0]}" >&2
            exit 1
        fi
    fi
    if [ ! -f "$arm64/gstreamer-app-1.0.pc" ]; then
        echo "ERROR: GStreamer Android SDK nie zawiera gstreamer-app-1.0.pc w $arm64" >&2
        exit 1
    fi
    armv7="$(find_gstreamer_android_pkg_config_dir 'armv7|armeabi|androideabi' true "${roots[@]}")" || armv7="$arm64"
    x64="$(find_gstreamer_android_pkg_config_dir 'x86_64' true "${roots[@]}")" || x64="$arm64"

    export PKG_CONFIG_ALLOW_CROSS=1
    export PKG_CONFIG_PATH_aarch64_linux_android="$arm64"
    export PKG_CONFIG_PATH_armv7_linux_androideabi="$armv7"
    export PKG_CONFIG_PATH_x86_64_linux_android="$x64"
    echo "GStreamer Android pkg-config arm64-v8a: $arm64"
    echo "GStreamer Android pkg-config armeabi-v7a: $armv7"
    echo "GStreamer Android pkg-config x86_64: $x64"
}

copy_android_dynamic_libs() {
    local platform="$1"
    local abi="$2"
    local src="$NATIVE_LIBS_DIR/$platform/lib-dynamic"
    local dst="$JNILIBS_DIR/$abi"
    mkdir -p "$dst"

    if [ -f "$src/libc++_shared.so" ]; then
        cp -f "$src/libc++_shared.so" "$dst/libc++_shared.so"
    fi

    if [ -f "$src/whisper-cpp/multi/libwhisper_tf.so" ]; then
        cp -f "$src/whisper-cpp/multi/libwhisper_tf.so" "$dst/libwhisper_tf.so"
    fi
}

native_libs_ready() {
    local platform="$1"
    [ -f "$NATIVE_LIBS_DIR/$platform/lib-static/libzvec_c_api.a" ] || return 1
    [ -f "$NATIVE_LIBS_DIR/$platform/lib-static/llama-cpp/multi/libllama.a" ] || return 1
    [ -f "$NATIVE_LIBS_DIR/$platform/lib-dynamic/whisper-cpp/multi/libwhisper_tf.so" ] || return 1
    [ -f "$NATIVE_LIBS_DIR/$platform/lib-dynamic/libc++_shared.so" ] || return 1
}

ensure_native_libs() {
    local abi platform
    for abi in "$@"; do
        platform="$(platform_for_abi "$abi")"
        if native_libs_ready "$platform"; then
            continue
        fi
        "$REPO_ROOT/scripts/native-libs/build-all-android.sh" --platform "$platform"
    done
}

BUILD_MODE="${1:-release}"
CARGO_FLAGS=""

if [ "$BUILD_MODE" = "release" ]; then
    CARGO_FLAGS="--release"
fi

configure_android_ndk_tools
configure_gstreamer_android

ABIS=()
CARGO_NDK_TARGETS=()
while IFS= read -r abi; do
    case "$abi" in
        arm64-v8a|armeabi-v7a|x86_64) ;;
        *) echo "Nieobslugiwany Android ABI: $abi" >&2; exit 1 ;;
    esac
    ABIS+=("$abi")
    CARGO_NDK_TARGETS+=("-t" "$abi")
    rustup target add "$(rust_target_for_abi "$abi")" 2>/dev/null || true
done < <(selected_android_abis)

ensure_native_libs "${ABIS[@]}"

echo "Building for Android targets..."
cd "$CORE_DIR"

cargo ndk \
    "${CARGO_NDK_TARGETS[@]}" \
    -o "$JNILIBS_DIR" \
    build $CARGO_FLAGS

for abi in "${ABIS[@]}"; do
    copy_android_dynamic_libs "$(platform_for_abi "$abi")" "$abi"
done

echo ""
echo "=== Build complete ==="
echo "JNI libraries placed in: $JNILIBS_DIR"
ls -la "$JNILIBS_DIR"/*/libtentaflow_mobile.so 2>/dev/null || echo "No .so files found yet"
