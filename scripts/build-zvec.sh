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
# Pin the stable v0.6.0 commit so every native target uses the same C API and
# reproduces the library paired with the vendored header.
ZVEC_REF="${ZVEC_REF:-ec8a78ee08b14a0b8c94158ffc1de42cd3f97f6d}"
PLATFORM="${1:-linux-x86_64}"

SRC_DIR="${ZVEC_SRC_DIR:-/tmp/zvec-build}"
ZVEC_PATCH_DIR="$ROOT/scripts/native-libs/patches/zvec"

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

android_abi_for_platform() {
  case "$1" in
    android-arm64) printf '%s\n' "arm64-v8a" ;;
    android-armv7) printf '%s\n' "armeabi-v7a" ;;
    android-x86_64) printf '%s\n' "x86_64" ;;
    *) return 1 ;;
  esac
}

apply_zvec_patches() {
  local patch
  for patch in "$ZVEC_PATCH_DIR"/*.patch; do
    [ -e "$patch" ] || continue
    git -C "$SRC_DIR" apply "$patch"
  done
}

merge_static_archive() {
  local output="$1"
  shift
  local ar_bin="${ANDROID_AR:-${AR:-ar}}"
  local mri
  mri="$(mktemp)"
  {
    printf 'create %s\n' "$output"
    local input
    for input in "$@"; do
      case "$input" in
        *.a|*.lib) printf 'addlib %s\n' "$input" ;;
        *) printf 'addmod %s\n' "$input" ;;
      esac
    done
    printf 'save\n'
    printf 'end\n'
  } > "$mri"
  "$ar_bin" -M < "$mri"
  rm -f "$mri"
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
  # Inny ZVEC_REF niz obecny checkout zostawia nieśledzone pliki w katalogach
  # submodulow thirdparty/ pod wersjonowanymi sciezkami (np. CRoaring-2.0.4/
  # LICENSE), ktore blokuja checkout. `git clean` pomija granice zainicjalizowanych
  # submodulow, wiec samo czyszczenie nie wystarcza — najpierw deinit (odrejestrowanie
  # + usuniecie ich working tree), potem clean, potem wymuszony checkout.
  if ! git checkout -f "$ZVEC_REF" 2>/dev/null; then
    git submodule deinit -f --all 2>/dev/null || true
    git clean -ffdxq 2>/dev/null || true
    git checkout -f "$ZVEC_REF"
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

apply_zvec_patches

OUT_LIB_DIR="$SYS_CRATE/vendor/lib/$PLATFORM"
# Zapewnij, ze katalogi docelowe naleza do biezacego usera. Jesli istnieja jako
# root-owned (np. po wczesniejszym sudo-buildzie albo klonie repo jako root w
# /opt), odzyskaj wlasnosc przez sudo chown — inaczej cp ponizej pada na
# "Permission denied". Uniwersalnie, niezaleznie od sposobu sklonowania repo.
for _dir in "$OUT_LIB_DIR" "$VENDOR_INCLUDE"; do
  if ! ( mkdir -p "$_dir" 2>/dev/null && [ -w "$_dir" ] ); then
    _owner="$(id -un):$(id -gn)"
    echo ">>> $_dir nie jest zapisywalny (root-owned?) — odzyskuje wlasnosc dla $_owner (sudo moze poprosic o haslo)..." >&2
    if command -v sudo >/dev/null 2>&1; then
      sudo mkdir -p "$_dir" || true
      sudo chown -R "$_owner" "$_dir" || true
    fi
    if [ ! -w "$_dir" ]; then
      echo "BLAD: nadal brak zapisu w $_dir. Uruchom: sudo chown -R $_owner \"$_dir\"" >&2
      exit 1
    fi
  fi
done

# Desktop builds link zvec's self-contained shared library (it bundles RocksDB/
# Arrow/protobuf + a static libstdc++ and exports only the C API). Mobile builds
# need a static archive instead — see the ios/android note below.

case "$PLATFORM" in
  linux-x86_64|linux-aarch64)
    # The shared lib statically bundles protobuf/abseil/RocksDB/Arrow AND a
    # static libstdc++. Without hiding them, those symbols are exported with
    # global visibility and INTERPOSE the same symbols in any other library the
    # host loads — notably the system onnxruntime used by the camera-CV engine,
    # whose protobuf/libstdc++ then bind to zvec's incompatible copies and
    # segfault. The version script exports only the `zvec_*` C API and localizes
    # everything else (same isolation we apply to the whisper dylib). Only the
    # final `zvec_c_api` shared object is built shared here (deps are static
    # archives), so a global SHARED_LINKER flag is safe.
    ZVEC_EXPORT_MAP="$SRC_DIR/tentaflow_zvec_exports.map"
    printf '{ global: zvec_*; local: *; };\n' > "$ZVEC_EXPORT_MAP"
    if linux_native_zvec_ok; then
      echo "  Build natywny: gcc-$(gcc-11 -dumpversion) / cmake $(cmake_major_version).x / ninja (bez Dockera)"
      ( cd "$SRC_DIR" && rm -rf build_zvec && mkdir -p build_zvec && cd build_zvec
        cmake -G Ninja -DCMAKE_BUILD_TYPE=Release \
          -DCMAKE_C_COMPILER=gcc-11 -DCMAKE_CXX_COMPILER=g++-11 \
          "-DCMAKE_SHARED_LINKER_FLAGS=-Wl,--version-script=$ZVEC_EXPORT_MAP" \
          -DBUILD_PYTHON_BINDINGS=OFF -DBUILD_TOOLS=OFF -DBUILD_TESTING=OFF -DBUILD_C_BINDINGS=ON ..
        ninja zvec_c_api -j"$(nproc)" )
    else
      echo "  Brak natywnego gcc-11/ninja/cmake<4 — buduje w Dockerze (Ubuntu 22.04/gcc-11)."
      # Wybor wywolania dockera: jesli biezacy user nie ma dostepu do socketu
      # (nie nalezy do grupy 'docker' ALBO dodano go do niej, ale zmiana wejdzie
      # dopiero po re-loginie), spadnij na 'sudo docker'. Artefakty i tak sa
      # chown-owane z powrotem do usera w kontenerze (ponizej), wiec nie zostaja
      # root-owned. Dzieki temu build-all.sh dziala bez wylogowywania sie.
      DOCKER="docker"
      if ! docker info >/dev/null 2>&1; then
        if command -v sudo >/dev/null 2>&1 && sudo docker info >/dev/null 2>&1; then
          echo "  (brak dostepu do socketu dockera dla biezacego usera — uzywam 'sudo docker';"
          echo "   aby dzialac bez sudo: 'sudo usermod -aG docker \$USER' i przeloguj sie / 'newgrp docker')"
          DOCKER="sudo docker"
        else
          echo "  BLAD: docker niedostepny (ani bezposrednio, ani przez sudo). Uruchom daemon dockera" >&2
          echo "  (sudo systemctl enable --now docker) albo dodaj sie do grupy 'docker' i przeloguj." >&2
          exit 1
        fi
      fi
      # `:z` na bind-mouncie jest WYMAGANE na hostach z SELinux (Fedora/RHEL):
      # bez relabelu SELinux odmawia kontenerowi zapisu do katalogu hosta nawet
      # jako root ("mkdir: Permission denied"). Na hostach bez SELinux Docker
      # ignoruje flage. `:z` (shared) zamiast `:Z` (private), zeby host po
      # zakonczeniu kontenera nadal mial dostep do artefaktow.
      $DOCKER run --rm --network=host -v "$SRC_DIR:/src:z" -w /src ubuntu:22.04 bash -c '
        set -e
        export DEBIAN_FRONTEND=noninteractive CC=gcc-11 CXX=g++-11
        apt-get update -qq && apt-get install -y -qq build-essential gcc-11 g++-11 git python3 python3-pip libssl-dev pkg-config curl ca-certificates >/dev/null 2>&1
        pip3 install -q "cmake<4" ninja >/dev/null 2>&1
        # Kontener dziala jako root, ale /src jest zamontowane z hosta i nalezy do
        # usera (inny uid) → git odmawia ("dubious ownership", CVE-2022-24765),
        # przez co RocksDB/cmake nie wykryja wersji przez git. Ufamy zamontowanym
        # repo (kontener jest --rm, wiec to per-build).
        git config --global --add safe.directory "*" 2>/dev/null || true
        # Osobne instrukcje (NIE łańcuch &&): w łańcuchu `A && B && C` set -e
        # pomija błędy A i B, więc nieudany mkdir przepuszczałby cmake z cwd /src
        # (źródło "..") → mylący "source directory / …". Tu każda linia przerywa.
        rm -rf build_zvec
        mkdir build_zvec
        cd build_zvec
        cmake -G Ninja -DCMAKE_BUILD_TYPE=Release -DCMAKE_C_COMPILER=gcc-11 -DCMAKE_CXX_COMPILER=g++-11 \
          "-DCMAKE_SHARED_LINKER_FLAGS=-Wl,--version-script=/src/tentaflow_zvec_exports.map" \
          -DBUILD_PYTHON_BINDINGS=OFF -DBUILD_TOOLS=OFF -DBUILD_TESTING=OFF -DBUILD_C_BINDINGS=ON ..
        ninja zvec_c_api -j"$(nproc)"
        chown -R '"$(id -u):$(id -g)"' /src/build_zvec
      '
    fi
    cp -f "$SRC_DIR/build_zvec/lib/libzvec_c_api.so" "$OUT_LIB_DIR/libzvec_c_api.so"
    ARTIFACT="$OUT_LIB_DIR/libzvec_c_api.so"
    ;;
  macos-arm64)
    ensure_cmake_pin_on_path
    ensure_apple_libtool_shim_on_program_path
    # Izolacja symboli: eksportuj WYŁĄCZNIE C API zvec (`_zvec_*`), schowaj
    # zbundlowane protobuf/abseil/RocksDB. macOS ld64 nie używa version-scriptów —
    # odpowiednikiem jest allowlista `-exported_symbols_list` (tylko wymienione
    # symbole są eksportowane). macOS two-level namespace i tak chroni przed
    # interpozycją, ale to domyka temat dwóch kopii protobuf „na zawsze".
    ZVEC_EXPORT_LIST="$SRC_DIR/tentaflow_zvec_exports_macos.txt"
    printf '_zvec_*\n' > "$ZVEC_EXPORT_LIST"
    ( cd "$SRC_DIR" && rm -rf build_zvec && mkdir -p build_zvec && cd build_zvec
      cmake -G Ninja -DCMAKE_BUILD_TYPE=Release -DBUILD_PYTHON_BINDINGS=OFF -DBUILD_TOOLS=OFF -DBUILD_TESTING=OFF -DBUILD_C_BINDINGS=ON \
        "-DCMAKE_SHARED_LINKER_FLAGS=-Wl,-exported_symbols_list,$ZVEC_EXPORT_LIST" ..
      ninja zvec_c_api -j"$(sysctl -n hw.ncpu)" )
    cp -f "$SRC_DIR/build_zvec/lib/libzvec_c_api.dylib" "$OUT_LIB_DIR/libzvec_c_api.dylib"
    ARTIFACT="$OUT_LIB_DIR/libzvec_c_api.dylib"
    ;;
  ios-arm64|ios-sim-arm64)
    # iOS jest celem cross-compile: appka nie moze wozic luznego .dylib, wiec
    # build.rs linkuje DWA statyczne archiwa (uklad "model B"):
    #   * libzvec_c_api.a  — wlasny kod zvec (binding C + 4 wewnetrzne biblioteki
    #     zvec (src/db) / zvec_core / zvec_ailego / zvec_turbo, ktore nosza rejestracje
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
    for name in zvec zvec_core zvec_ailego zvec_turbo; do
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
            libzvec.a|libzvec_*.a) continue ;;          # kod zvec jest juz w 4 PACKED bibliotekach
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
  android-*)
    ANDROID_ABI="$(android_abi_for_platform "$PLATFORM")"
    ANDROID_API_LEVEL="${ANDROID_API_LEVEL:-26}"
    ANDROID_SDK_ROOT="${ANDROID_SDK_ROOT:-${ANDROID_HOME:-/opt/android-sdk}}"
    ANDROID_NDK_HOME="${ANDROID_NDK_HOME:-${ANDROID_NDK_ROOT:-/opt/android-ndk}}"
    ANDROID_TOOLCHAIN="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin"
    if [ ! -f "$ANDROID_NDK_HOME/build/cmake/android.toolchain.cmake" ]; then
        echo "Build Android zvec wymaga Android NDK (ustaw ANDROID_NDK_HOME)." >&2
        exit 1
    fi
    if [ ! -x "$ANDROID_TOOLCHAIN/llvm-ar" ]; then
        echo "Nie znaleziono llvm-ar w Android NDK: $ANDROID_TOOLCHAIN" >&2
        exit 1
    fi
    export ANDROID_SDK_ROOT ANDROID_HOME="$ANDROID_SDK_ROOT" ANDROID_NDK_HOME
    export ANDROID_AR="$ANDROID_TOOLCHAIN/llvm-ar"
    unset RUSTC_WRAPPER
    unset CMAKE_C_COMPILER_LAUNCHER
    unset CMAKE_CXX_COMPILER_LAUNCHER
    unset CMAKE_CUDA_COMPILER_LAUNCHER
    unset CMAKE_HIP_COMPILER_LAUNCHER
    export CCACHE_DISABLE=1
    export SCCACHE_DISABLE=1

    ensure_cmake_pin_on_path

    echo "  [1/3] protoc dla hosta..."
    HOST_BUILD="$SRC_DIR/build_host"
    ( cd "$SRC_DIR" && mkdir -p build_host && cd build_host
      env \
        -u CMAKE_C_COMPILER_LAUNCHER \
        -u CMAKE_CXX_COMPILER_LAUNCHER \
        cmake -G Ninja \
        -DCMAKE_BUILD_TYPE=Release \
        -DCMAKE_C_COMPILER_LAUNCHER= \
        -DCMAKE_CXX_COMPILER_LAUNCHER= \
        -DBUILD_PYTHON_BINDINGS=OFF \
        -DBUILD_TOOLS=OFF \
        -DBUILD_TESTING=OFF \
        ..
      ninja protoc -j"$(nproc 2>/dev/null || echo 4)" )
    PROTOC_BIN="$(find "$HOST_BUILD" \( -name protoc -o -name 'protoc-*' \) -type f -perm -111 | head -n1)"
    if [ -z "$PROTOC_BIN" ]; then
        echo "Nie znaleziono zbudowanego protoc w $HOST_BUILD"
        exit 1
    fi

    echo "  reset thirdparty (host -> Android)..."
    ( cd "$SRC_DIR" && git submodule foreach --recursive \
        'git checkout -q -- . 2>/dev/null || true; git clean -fdxq' >/dev/null 2>&1 || true )

    echo "  [2/3] cross-build zvec ($PLATFORM, ABI $ANDROID_ABI, API $ANDROID_API_LEVEL)..."
    ANDROID_BUILD="$SRC_DIR/build_android_${ANDROID_ABI}"
    rm -rf "$ANDROID_BUILD"
    ( cd "$SRC_DIR" && rm -rf "build_android_${ANDROID_ABI}" && mkdir -p "build_android_${ANDROID_ABI}" && cd "build_android_${ANDROID_ABI}"
      env \
        -u CMAKE_C_COMPILER_LAUNCHER \
        -u CMAKE_CXX_COMPILER_LAUNCHER \
        cmake -G Ninja \
        -DANDROID_NDK="$ANDROID_NDK_HOME" \
        -DCMAKE_TOOLCHAIN_FILE="$ANDROID_NDK_HOME/build/cmake/android.toolchain.cmake" \
        -DANDROID_ABI="$ANDROID_ABI" \
        -DANDROID_NATIVE_API_LEVEL="$ANDROID_API_LEVEL" \
        -DANDROID_STL="c++_static" \
        -DCMAKE_BUILD_TYPE=Release \
        -DCMAKE_C_COMPILER_LAUNCHER= \
        -DCMAKE_CXX_COMPILER_LAUNCHER= \
        -DBUILD_PYTHON_BINDINGS=OFF \
        -DBUILD_TOOLS=OFF \
        -DBUILD_TESTING=OFF \
        -DBUILD_C_BINDINGS=ON \
        -DENABLE_NATIVE=OFF \
        -DAUTO_DETECT_ARCH=OFF \
        -DGLOBAL_CC_PROTOBUF_PROTOC="$PROTOC_BIN" \
        ..
      ninja zvec_c_api -j"$(nproc 2>/dev/null || echo 4)" )

    echo "  [3/3] scalanie archiwow (llvm-ar MRI)..."
    OWN_LIBS=()
    for name in zvec zvec_core zvec_ailego zvec_turbo; do
        a="$ANDROID_BUILD/lib/lib${name}.a"
        [ -f "$a" ] || a="$(find "$ANDROID_BUILD" -name "lib${name}.a" -type f | head -n1)"
        if [ -z "$a" ] || [ ! -f "$a" ]; then
            echo "Brak wewnetrznej biblioteki lib${name}.a w $ANDROID_BUILD"
            exit 1
        fi
        OWN_LIBS+=("$a")
    done
    CAPI_OBJ="$(find "$ANDROID_BUILD" -path '*zvec_c_api*' -name 'c_api.cc.o' -type f | head -n1)"
    if [ -z "$CAPI_OBJ" ]; then
        echo "Nie znaleziono obiektu bindingu C (c_api.cc.o) w $ANDROID_BUILD"
        exit 1
    fi

    DEP_LIBS=()
    seen_bases=""
    while IFS= read -r a; do
        base="$(basename "$a")"
        case "$base" in
            libzvec.a|libzvec_*.a) continue ;;
        esac
        case "$seen_bases" in
            *"|$base|"*) continue ;;
        esac
        seen_bases="${seen_bases}|$base|"
        DEP_LIBS+=("$a")
    done < <(find "$ANDROID_BUILD" -name '*.a' -type f | sort)
    if [ "${#DEP_LIBS[@]}" -eq 0 ]; then
        echo "Nie znaleziono archiwow third-party w $ANDROID_BUILD"
        exit 1
    fi

    merge_static_archive "$OUT_LIB_DIR/libzvec_c_api.a" "${OWN_LIBS[@]}" "$CAPI_OBJ"
    merge_static_archive "$OUT_LIB_DIR/libzvec_deps.a" "${DEP_LIBS[@]}"
    ARTIFACT="$OUT_LIB_DIR/libzvec_c_api.a"
    echo "  deps scalone z ${#DEP_LIBS[@]} archiwow third-party"
    ;;
  *)
    echo "Unknown platform: $PLATFORM"
    exit 1
    ;;
esac

# Vendor the header (committed) — keep it in sync with the built lib.
cp -f "$SRC_DIR/src/include/zvec/c_api.h" "$VENDOR_INCLUDE/c_api.h"

echo ""
echo "=========================================="
echo "  Done: $ARTIFACT ($(du -h "$ARTIFACT" | cut -f1))"
echo "  Header: $VENDOR_INCLUDE/c_api.h"
echo "=========================================="
