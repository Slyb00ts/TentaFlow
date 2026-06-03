#!/bin/bash
# =============================================================================
# File: build-zvec.sh
# Purpose: Build the zvec embedded vector DB as ONE self-contained static archive
#          (libzvec_c_api.a) per platform and vendor it into tentaflow-zvec-sys.
#
# zvec depends on RocksDB + Arrow + protobuf + ANTLR etc. We compile it once with
# a pinned, known-good toolchain (Linux: Ubuntu 22.04 / gcc-11 in Docker, because
# RocksDB 8.1 does not build with very new GCC), then merge every component +
# third-party .a into a single archive (the "model B" layout). The archive is
# gitignored; this script regenerates it.
#
# Usage:
#   ./scripts/build-zvec.sh linux-x86_64       # Docker build (default)
#   ./scripts/build-zvec.sh macos-arm64        # native, must run on macOS
#   ./scripts/build-zvec.sh ios-arm64          # native, must run on macOS (Xcode)
# =============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
SYS_CRATE="$ROOT/tentaflow-zvec-sys"
VENDOR_INCLUDE="$SYS_CRATE/vendor/include/zvec"

ZVEC_REPO="https://github.com/alibaba/zvec"
# zvec FTS/hybrid-search API (zvec_fts_*, reranker, multi_query) wszedl po tagu
# v0.4.0 (commit 02bfb31 #408) i nie ma go w zadnym tagu. Wrapper tentaflow-zvec
# go uzywa, wiec pinujemy konkretny commit main, ktorego c_api.h zgadza sie z
# zwendorowanym naglowkiem.
ZVEC_REF="${ZVEC_REF:-f562bdd636d454f18128cb18b41578128d1415a4}"
PLATFORM="${1:-linux-x86_64}"

SRC_DIR="${ZVEC_SRC_DIR:-/tmp/zvec-build}"

# zvec submoduly wymagaja cmake<4 (CMake 4 wywalil kompatybilnosc z
# cmake_minimum_required<3.5); zwraca major cmake albo nic.
cmake_major_version() { cmake --version 2>/dev/null | sed -n '1s/.*version \([0-9][0-9]*\).*/\1/p'; }

# True (0) gdy zvec mozna zbudowac na Linuksie NATYWNIE (bez Dockera): potrzeba
# gcc-11/g++-11 (RocksDB 8.1 nie kompiluje sie pod gcc>=13), ninja oraz cmake<4.
linux_native_zvec_ok() {
  command -v gcc-11 >/dev/null 2>&1 || return 1
  command -v g++-11 >/dev/null 2>&1 || return 1
  command -v ninja  >/dev/null 2>&1 || return 1
  local maj; maj="$(cmake_major_version)"
  [ -n "$maj" ] && [ "$maj" -lt 4 ] || return 1
  return 0
}

# zvec submoduly (googletest/RocksDB/Arrow) wymagaja cmake<4 (CMake 4 usunal
# kompatybilnosc z cmake_minimum_required<3.5, a wymuszenie starej polityki psuje
# eksport include-dirs Arrow). Pinujemy cmake 3.x w lokalnym venv i wystawiamy go
# na PATH (uzywane przez build macOS i iOS — oba sa Apple/Xcode).
ensure_cmake_pin_on_path() {
  if ! command -v python3 >/dev/null 2>&1; then
    echo "python3 nie znaleziony — wymagany do przypiecia cmake<4 (zainstaluj Xcode Command Line Tools)."
    exit 1
  fi
  local pin="$SRC_DIR/.cmake-pin"
  if [ ! -x "$pin/bin/cmake" ]; then
    python3 -m venv "$pin"
    "$pin/bin/pip" install -q "cmake<4"
  fi
  PATH="$pin/bin:$PATH"
}

# Xcode 26 zmienil string wersji Apple libtool z "cctools-<n>" na "cctools_ld-<n>".
# Arrow 21 (arrow_create_merged_static_lib) sprawdza `libtool -V` regexem
# `cctools-([0-9.]+)` i blednie odrzuca prawdziwy Apple libtool ("appears not to be
# Apple's libtool"), choc samo narzedzie dziala. Podstawiamy shim `libtool`, ktory na
# `-V` zwraca string zgodny ze starym formatem, a kazde inne wywolanie deleguje do
# /usr/bin/libtool. Arrow znajduje shim przez ENV CMAKE_PROGRAM_PATH (przeszukiwane w
# find_program PRZED HINTS /usr/bin). Dotyczy buildow Apple (macOS + iOS).
ensure_apple_libtool_shim_on_program_path() {
  local shim_dir="$SRC_DIR/.libtool-shim"
  mkdir -p "$shim_dir"
  cat > "$shim_dir/libtool" <<'SHIM'
#!/bin/bash
if [ "$1" = "-V" ]; then
  out="$(/usr/bin/libtool -V 2>/dev/null)"
  printf '%s\n' "${out/cctools_ld-/cctools-}"
  exit 0
fi
exec /usr/bin/libtool "$@"
SHIM
  chmod +x "$shim_dir/libtool"
  export CMAKE_PROGRAM_PATH="$shim_dir${CMAKE_PROGRAM_PATH:+:$CMAKE_PROGRAM_PATH}"
}

echo "=========================================="
echo "  Build zvec static archive (model B)"
echo "  Platform: $PLATFORM | ref: $ZVEC_REF"
echo "  Source:   $SRC_DIR"
echo "=========================================="

# 1. Source + submodules (RocksDB/Arrow/protobuf/...) at the pinned tag.
if [ ! -d "$SRC_DIR/.git" ]; then
    git clone "$ZVEC_REPO" "$SRC_DIR"
fi
( cd "$SRC_DIR" && git fetch origin -q
  # Inny ZVEC_REF niz obecny checkout moze ciagnac inne wersje thirdparty/
  # submodulow; zostawione untracked pliki (rozpakowane zaleznosci, np. CRoaring)
  # blokuja checkout. Sprobuj normalnie; gdy sie nie uda — wyczysc drzewo +
  # submoduly (untracked tez) i ponow.
  if ! git checkout "$ZVEC_REF" 2>/dev/null; then
    git submodule foreach --recursive 'git reset --hard -q; git clean -ffdxq' 2>/dev/null || true
    git reset --hard -q 2>/dev/null || true
    git clean -ffdxq 2>/dev/null || true
    git checkout "$ZVEC_REF"
  fi
  git submodule update --init --recursive --depth 1 --force
  # zvec laplikuje patche na thirdparty (glog/arrow/antlr) przez apply_patch_once,
  # ktory pilnuje sie UNTRACKED markerem .<name>_patched. `git submodule update
  # --force` cofa zaplatany kod do recorded commitu, ale marker zostaje — wiec przy
  # kolejnym buildzie patch NIE jest nakladany, a CMake generate pada (np. glog
  # eksportuje glog-targets z gflags_nothreads_static obecnym w dwoch zestawach
  # eksportu). Czyscimy untracked w submodulach, by patche nalozyly sie od nowa na
  # czyste zrodla.
  git submodule foreach --recursive 'git clean -fdxq' 2>/dev/null || true )

OUT_LIB_DIR="$SYS_CRATE/vendor/lib/$PLATFORM"
mkdir -p "$OUT_LIB_DIR" "$VENDOR_INCLUDE"

# Desktop builds link zvec's self-contained shared library (it bundles RocksDB/
# Arrow/protobuf + a static libstdc++ and exports only the C API). Mobile builds
# need a static archive instead — see the ios/android note below.

case "$PLATFORM" in
  linux-x86_64|linux-aarch64)
    # RocksDB 8.1 nie kompiluje sie pod gcc>=13. Preferujemy NATYWNY gcc-11
    # (bez Dockera, bez root-owned artefaktow); gdy go brak — fallback na
    # kontener Ubuntu 22.04/gcc-11 (dostarcza gcc-11 + cmake<4 w srodku).
    if linux_native_zvec_ok; then
      echo "  Build natywny: gcc-$(gcc-11 -dumpversion) / cmake $(cmake_major_version).x / ninja (bez Dockera)"
      ( cd "$SRC_DIR" && rm -rf build_zvec && mkdir -p build_zvec && cd build_zvec
        cmake -G Ninja -DCMAKE_BUILD_TYPE=Release \
          -DCMAKE_C_COMPILER=gcc-11 -DCMAKE_CXX_COMPILER=g++-11 \
          -DBUILD_PYTHON_BINDINGS=OFF -DBUILD_TOOLS=OFF -DBUILD_TESTING=OFF -DBUILD_C_BINDINGS=ON ..
        ninja zvec_c_api -j"$(nproc)" )
    else
      echo "  Brak natywnego gcc-11/ninja/cmake<4 — buduje w Dockerze (Ubuntu 22.04/gcc-11)."
      docker run --rm -v "$SRC_DIR:/src" -w /src ubuntu:22.04 bash -c '
        set -e
        export DEBIAN_FRONTEND=noninteractive CC=gcc-11 CXX=g++-11
        apt-get update -qq && apt-get install -y -qq build-essential gcc-11 g++-11 git python3 python3-pip libssl-dev pkg-config curl ca-certificates >/dev/null 2>&1
        pip3 install -q "cmake<4" ninja >/dev/null 2>&1
        rm -rf build_zvec && mkdir -p build_zvec && cd build_zvec
        cmake -G Ninja -DCMAKE_BUILD_TYPE=Release -DCMAKE_C_COMPILER=gcc-11 -DCMAKE_CXX_COMPILER=g++-11 \
          -DBUILD_PYTHON_BINDINGS=OFF -DBUILD_TOOLS=OFF -DBUILD_TESTING=OFF -DBUILD_C_BINDINGS=ON ..
        ninja zvec_c_api -j"$(nproc)"
        chown -R '"$(id -u):$(id -g)"' /src/build_zvec
      '
    fi
    cp "$SRC_DIR/build_zvec/lib/libzvec_c_api.so" "$OUT_LIB_DIR/libzvec_c_api.so"
    ARTIFACT="$OUT_LIB_DIR/libzvec_c_api.so"
    ;;
  macos-arm64)
    ensure_cmake_pin_on_path
    ensure_apple_libtool_shim_on_program_path
    ( cd "$SRC_DIR" && rm -rf build_zvec && mkdir -p build_zvec && cd build_zvec
      cmake -G Ninja -DCMAKE_BUILD_TYPE=Release -DBUILD_PYTHON_BINDINGS=OFF -DBUILD_TOOLS=OFF -DBUILD_TESTING=OFF -DBUILD_C_BINDINGS=ON ..
      ninja zvec_c_api -j"$(sysctl -n hw.ncpu)" )
    cp "$SRC_DIR/build_zvec/lib/libzvec_c_api.dylib" "$OUT_LIB_DIR/libzvec_c_api.dylib"
    ARTIFACT="$OUT_LIB_DIR/libzvec_c_api.dylib"
    ;;
  ios-arm64|ios-sim-arm64)
    # iOS jest celem cross-compile: appka nie moze wozic luznego .dylib, wiec
    # build.rs linkuje DWA statyczne archiwa (uklad "model B"):
    #   * libzvec_c_api.a  — wlasny kod zvec (binding C + 4 wewnetrzne biblioteki
    #     zvec_db/zvec_core/zvec_ailego/zvec_turbo, ktore nosza rejestracje
    #     static-init indeksow/metryk). Linkowane przez whole-archive.
    #   * libzvec_deps.a   — third-party (protobuf/Arrow/RocksDB/...). Zwykly link.
    # zvec na iOS buduje wszystkie biblioteki statycznie (_add_library: na IOS glowny
    # target tez jest STATIC), a archiwa laduja w build/lib. Scalamy je tu przez
    # `libtool -static` (Apple ar nie laczy wielu .a w jedno).
    if [ "$(uname -s)" != "Darwin" ]; then
        echo "Build iOS ($PLATFORM) wymaga macOS + Xcode."
        exit 1
    fi
    if [ "$PLATFORM" = "ios-arm64" ]; then
        IOS_SDK="iphoneos"
    else
        IOS_SDK="iphonesimulator"
    fi
    IOS_ARCH="arm64"
    IOS_DEPLOYMENT_TARGET="13.0"
    SDK_PATH="$(xcrun --sdk "$IOS_SDK" --show-sdk-path)"

    ensure_cmake_pin_on_path
    ensure_apple_libtool_shim_on_program_path

    # Krok 1: protoc dla HOSTA. Cross-build odpala protoc w trakcie kompilacji,
    # wiec potrzebny jest binarny protoc dzialajacy na macOS (nie na iOS).
    echo "  [1/3] protoc dla hosta (macOS)..."
    HOST_BUILD="$SRC_DIR/build_host"
    ( cd "$SRC_DIR" && mkdir -p build_host && cd build_host
      cmake -G Ninja -DCMAKE_BUILD_TYPE=Release -DBUILD_PYTHON_BINDINGS=OFF -DBUILD_TOOLS=OFF -DBUILD_TESTING=OFF ..
      ninja protoc -j"$(sysctl -n hw.ncpu)" )
    # protoc laduje jako bin/protoc-<wersja> (realny plik) + bin/protoc (symlink);
    # bierzemy realny wykonywalny plik (symlink odpada przez -type f).
    PROTOC_BIN="$(find "$HOST_BUILD" \( -name protoc -o -name 'protoc-*' \) -type f -perm -111 | head -n1)"
    if [ -z "$PROTOC_BIN" ]; then
        echo "Nie znaleziono zbudowanego protoc w $HOST_BUILD"
        exit 1
    fi

    # Reset thirdparty miedzy buildem hosta a iOS: build hosta (non-IOS) naklada
    # arrow.patch (arrow_fix), a konfiguracja iOS arrow.ios.patch (arrow_ios_fix) na
    # TO SAMO zrodlo arrow — obu naraz nalozyc sie nie da. Cofamy patche (git checkout)
    # i kasujemy markery apply_patch_once (git clean), by konfiguracja iOS nalozyla
    # swoje patche na czyste zrodla (jak zvec/scripts/build_ios.sh robi przez stash).
    echo "  reset thirdparty (host -> iOS)..."
    ( cd "$SRC_DIR" && git submodule foreach --recursive \
        'git checkout -q -- . 2>/dev/null || true; git clean -fdxq' >/dev/null 2>&1 || true )

    # Krok 2: cross-build zvec na iOS SDK.
    echo "  [2/3] cross-build zvec ($PLATFORM, SDK $IOS_SDK)..."
    IOS_BUILD="$SRC_DIR/build_ios_${PLATFORM}"
    # Generator: Unix Makefiles, NIE Ninja. Na iOS zvec (_add_library) tworzy cel
    # ${NAME} ORAZ ${NAME}_static — oba STATIC z tym samym OUTPUT_NAME, wiec oba pisza
    # lib${NAME}.a. Ninja odrzuca to przy generate ("multiple rules generate ..."),
    # a Makefiles to toleruje (tak buduje upstreamowy zvec/scripts/build_ios.sh).
    ( cd "$SRC_DIR" && rm -rf "build_ios_${PLATFORM}" && mkdir -p "build_ios_${PLATFORM}" && cd "build_ios_${PLATFORM}"
      cmake \
        -DCMAKE_SYSTEM_NAME=iOS \
        -DCMAKE_OSX_DEPLOYMENT_TARGET="$IOS_DEPLOYMENT_TARGET" \
        -DCMAKE_OSX_ARCHITECTURES="$IOS_ARCH" \
        -DCMAKE_OSX_SYSROOT="$SDK_PATH" \
        -DCMAKE_BUILD_TYPE=Release \
        -DBUILD_PYTHON_BINDINGS=OFF \
        -DBUILD_TOOLS=OFF \
        -DBUILD_TESTING=OFF \
        -DBUILD_C_BINDINGS=ON \
        -DGLOBAL_CC_PROTOBUF_PROTOC="$PROTOC_BIN" \
        -DIOS=ON \
        ..
      make zvec_c_api -j"$(sysctl -n hw.ncpu)" )

    # Krok 3: scal archiwa w dwa wynikowe pliki.
    echo "  [3/3] scalanie archiwow (libtool -static)..."
    OWN_LIBS=()
    for name in zvec_db zvec_core zvec_ailego zvec_turbo; do
        a="$IOS_BUILD/lib/lib${name}.a"
        [ -f "$a" ] || a="$(find "$IOS_BUILD" -name "lib${name}.a" -type f | head -n1)"
        if [ -z "$a" ] || [ ! -f "$a" ]; then
            echo "Brak wewnetrznej biblioteki lib${name}.a w $IOS_BUILD"
            exit 1
        fi
        OWN_LIBS+=("$a")
    done
    # Obiekt bindingu C (cel zvec_c_api nie ma wariantu .a — bierzemy jego .o).
    CAPI_OBJ="$(find "$IOS_BUILD" -path '*zvec_c_api*' -name 'c_api.cc.o' -type f | head -n1)"
    if [ -z "$CAPI_OBJ" ]; then
        echo "Nie znaleziono obiektu bindingu C (c_api.cc.o) w $IOS_BUILD"
        exit 1
    fi

    # Third-party (.a) = wszystko OPROCZ wewnetrznych libzvec_*.a; dedup po nazwie.
    # (bash 3.2 na macOS nie ma tablic asocjacyjnych — dedup po liscie nazw.)
    DEP_LIBS=()
    seen_bases=""
    while IFS= read -r a; do
        base="$(basename "$a")"
        case "$base" in
            libzvec_*.a) continue ;;          # kod zvec jest juz w 4 PACKED bibliotekach
        esac
        case "$seen_bases" in
            *"|$base|"*) continue ;;
        esac
        seen_bases="${seen_bases}|$base|"
        DEP_LIBS+=("$a")
    done < <(find "$IOS_BUILD" -name '*.a' -type f | sort)
    if [ "${#DEP_LIBS[@]}" -eq 0 ]; then
        echo "Nie znaleziono zadnych archiwow third-party w $IOS_BUILD"
        exit 1
    fi

    libtool -static -no_warning_for_no_symbols \
        -o "$OUT_LIB_DIR/libzvec_c_api.a" "${OWN_LIBS[@]}" "$CAPI_OBJ"
    libtool -static -no_warning_for_no_symbols \
        -o "$OUT_LIB_DIR/libzvec_deps.a" "${DEP_LIBS[@]}"
    ARTIFACT="$OUT_LIB_DIR/libzvec_c_api.a"
    echo "  deps scalone z ${#DEP_LIBS[@]} archiwow third-party"
    ;;
  android-arm64)
    echo "Mobile ($PLATFORM) needs a STATIC build and must run on the platform SDK:"
    echo "  Android: zvec/scripts/build_android.sh (NDK)"
    echo "Then vendor libzvec_c_api.a (+ libzvec_deps.a) into $OUT_LIB_DIR."
    exit 1
    ;;
  *)
    echo "Unknown platform: $PLATFORM"
    exit 1
    ;;
esac

# Vendor the header (committed) — keep it in sync with the built lib.
cp "$SRC_DIR/src/include/zvec/c_api.h" "$VENDOR_INCLUDE/c_api.h"

echo ""
echo "=========================================="
echo "  Done: $ARTIFACT ($(du -h "$ARTIFACT" | cut -f1))"
echo "  Header: $VENDOR_INCLUDE/c_api.h"
echo "=========================================="
