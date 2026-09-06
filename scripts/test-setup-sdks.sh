#!/usr/bin/env bash
# ============ File: test-setup-sdks.sh - Check SDK discovery without modifying the host. ============
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
source "$ROOT/scripts/setup.sh"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
export HOME="$WORK/home"
mkdir -p "$HOME"
unset WASI_SDK_PATH TENTAFLOW_NATIVE_CACHE XDG_CACHE_HOME

if wasi_sdk_path; then
    echo 'Empty cache must not resolve an SDK' >&2
    exit 1
fi
mkdir -p "$HOME/.cache/tentaflow-native-libs/wasi-sdk-25.0/share/wasi-sysroot"
[[ $(wasi_sdk_path) == "$HOME/.cache/tentaflow-native-libs/wasi-sdk-25.0" ]]
export XDG_CACHE_HOME="$WORK/xdg cache"
mkdir -p "$XDG_CACHE_HOME/tentaflow-native-libs/wasi-sdk-26.0/share/wasi-sysroot"
[[ $(wasi_sdk_path) == "$XDG_CACHE_HOME/tentaflow-native-libs/wasi-sdk-26.0" ]]
export TENTAFLOW_NATIVE_CACHE="$WORK/native cache"
mkdir -p "$TENTAFLOW_NATIVE_CACHE/wasi-sdk-25.0/share/wasi-sysroot"
mkdir -p "$TENTAFLOW_NATIVE_CACHE/wasi-sdk-26.0/share/wasi-sysroot"
mkdir -p "$TENTAFLOW_NATIVE_CACHE/wasi-sdk-27.0"
[[ $(wasi_sdk_path) == "$TENTAFLOW_NATIVE_CACHE/wasi-sdk-26.0" ]]
export WASI_SDK_PATH="$WORK/custom sdk"
[[ $(wasi_sdk_path) == "$WASI_SDK_PATH" ]]
if install_wasi_sdk; then
    echo 'Invalid explicit SDK must fail without downloading a replacement' >&2
    exit 1
fi
unset WASI_SDK_PATH
if verify_wasi_sdk "$(wasi_sdk_path)"; then
    echo 'A sysroot without a compiler must fail verification' >&2
    exit 1
fi

# Simulate the SDK inventory, not an actual compiler or publish operation.
dotnet() { printf '%s\n' '9.0.100 [/sdk]' '11.0.100 [/sdk]'; }
if has_dotnet_sdk; then
    echo 'SDK inventory without .NET 10 must fail' >&2
    exit 1
fi
dotnet() { printf '%s\n' '9.0.100 [/sdk]' '10.0.100 [/sdk]'; }
has_dotnet_sdk
install_dotnet_sdk
[[ ! -e "$HOME/.dotnet" ]]

unset -f dotnet
for host in Darwin Linux; do
    for machine in x86_64 aarch64; do
        export TENTAFLOW_NATIVE_CACHE="$WORK/download-$host-$machine"
        uname() {
            case "$1" in -s) echo "$host" ;; -m) echo "$machine" ;; esac
        }
        curl() { printf '%s\n' "$@" > "$WORK/curl-args"; return 22; }
        if install_wasi_sdk; then
            echo 'Download failures must fail installation' >&2
            exit 1
        fi
        os=linux; [[ "$host" != Darwin ]] || os=macos
        arch=arm64; [[ "$machine" != x86_64 ]] || arch=x86_64
        grep -Fqx "https://github.com/WebAssembly/wasi-sdk/releases/download/wasi-sdk-25/wasi-sdk-25.0-$arch-$os.tar.gz" "$WORK/curl-args"
        [[ -z $(ls -A "$TENTAFLOW_NATIVE_CACHE") ]]
    done
done
unset -f uname curl

echo 'PASS: SDK discovery, cache precedence, invalid SDK rejection, .NET version detection'
echo 'PASS: download URLs for four hosts and cleanup after download failures'

if [[ "${TENTAFLOW_TEST_SDK_DOWNLOADS:-0}" == 1 ]]; then
    # Force an isolated, real installation even when the host already has .NET.
    dotnet() { "$HOME/.dotnet/dotnet" "$@"; }
    DISTRO=macos
    export TENTAFLOW_NATIVE_CACHE="$WORK/real-sdk"
    install_dotnet_sdk
    install_wasi_sdk
    install_dotnet_sdk
    install_wasi_sdk
    verify_wasi_sdk "$(wasi_sdk_path)"
    [[ $(grep -Fxc '. "$HOME/.dotnet/env"' "$HOME/.zprofile") == 1 ]]
    [[ $(grep -Fxc '. "$HOME/.dotnet/env"' "$HOME/.bashrc") == 1 ]]
    echo 'PASS: real SDK installation, WASM compile/link and repeated installation'
fi
