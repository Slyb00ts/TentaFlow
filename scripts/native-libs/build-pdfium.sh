#!/usr/bin/env bash
# =============================================================================
# Plik: scripts/native-libs/build-pdfium.sh
# Opis: Pobiera PREBUILT libpdfium (Google PDFium, BSD-3) z bblanchon/
#       pdfium-binaries (pakiet MIT) i zapisuje go w native-libs/<platform>/
#       lib-dynamic. Wariant NON-V8 (bez JS) — doc_parse rasteryzuje tylko
#       strony do RGB, silnik JS jest zbędny i zwiększałby powierzchnię ataku.
#       NIE buduje z C++ źródeł — to gotowy artefakt.
# =============================================================================

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/common.sh"

PLATFORM="${1:-$(detect_platform)}"
prepare_layout "$PLATFORM"

# Release tag bblanchon/pdfium-binaries. `chromium/<n>` to numer gałęzi
# Chromium, z której zbudowano pdfium. Override: PDFIUM_RELEASE=chromium/NNNN.
PINNED_RELEASE="chromium/7891"
PDFIUM_RELEASE="${PDFIUM_RELEASE:-$PINNED_RELEASE}"

# Mapowanie platformy TentaFlow -> (asset, libname, SHA256 oczekiwane dla
# PINNED_RELEASE). bblanchon NIE publikuje sidecarów .sha256 w release, więc
# sumy policzono lokalnie z pobranych artefaktów chromium/7891 i zapisano jako
# known-good (pin integralności prebuiltu — patrz weryfikacja niżej).
case "$PLATFORM" in
  linux-x86_64)   ASSET="pdfium-linux-x64.tgz";        LIBNAME="libpdfium.so";    SHA256="e21257c643592dc8eaf284f5f54cd7eca5e1694ff35a5b2158a351931bea107f" ;;
  linux-aarch64)  ASSET="pdfium-linux-arm64.tgz";      LIBNAME="libpdfium.so";    SHA256="727cff9203e18a1861b1b2c107aee7f981a96e690b628f7f36ff4a84d1a992a4" ;;
  macos-x86_64)   ASSET="pdfium-mac-x64.tgz";          LIBNAME="libpdfium.dylib"; SHA256="785c4fce5ca1d7bbd4c2d07fcb3f5adb1dcd1ceba37e1e1fd0b2fd70875481d6" ;;
  macos-arm64)    ASSET="pdfium-mac-arm64.tgz";        LIBNAME="libpdfium.dylib"; SHA256="95d44263629eb8d0f6a619d5443da1ed449d4f916b26f4df1878ad4d5a64b0fc" ;;
  windows-x86_64) ASSET="pdfium-win-x64.tgz";          LIBNAME="pdfium.dll";      SHA256="1a5b95fde0eb446a5709ffc4a2e6691fa2b5ace224cee20dddd16c110c5ce60e" ;;
  android-arm64)  ASSET="pdfium-android-arm64.tgz";    LIBNAME="libpdfium.so";    SHA256="e6269bfdbb8bd92bca563847604bb2ef9e88d787bbcfeda2c1dfabf3fdc72d26" ;;
  android-armv7)  ASSET="pdfium-android-arm.tgz";      LIBNAME="libpdfium.so";    SHA256="ed9f574a8fffee3a7c69bad1aab99ca2f12ea7eecfdf79d3501cd4c877cfa895" ;;
  android-x86_64) ASSET="pdfium-android-x64.tgz";      LIBNAME="libpdfium.so";    SHA256="8dddb46b7f419ee1d1f99e88d272a38cb1b1aee497f7154c1c2de999fa6c57bd" ;;
  ios-arm64)      ASSET="pdfium-ios-device-arm64.tgz"; LIBNAME="libpdfium.dylib"; SHA256="cbf527de8f3a3d3cf8ea35b1e76ae3e5bae9475485c5d828af242b576da81fde" ;;
  *)
    echo "Nieobsługiwana platforma pdfium: $PLATFORM" >&2
    exit 1
    ;;
esac

BASE_URL="https://github.com/bblanchon/pdfium-binaries/releases/download/${PDFIUM_RELEASE}"
URL="${BASE_URL}/${ASSET}"

WORK="$NATIVE_CACHE/build/pdfium/$PLATFORM"
reset_dir "$WORK"
ARCHIVE="$WORK/$ASSET"

echo ">>> Pobieram prebuilt pdfium: $URL"
require_cmd curl tar
curl -fsSL "$URL" -o "$ARCHIVE"

# Bug 3: weryfikacja SHA256 prebuiltu PRZED ekstrakcją. Pin integralności —
# bez tego MITM/zatruty cache mógłby podmienić bibliotekę ładowaną runtime'em.
# Override PDFIUM_RELEASE na inny tag => suma known-good nie pasuje; ostrzegamy
# i pomijamy weryfikację (dev), chyba że dodatkowo wymuszono PDFIUM_SKIP_CHECKSUM.
if [ -n "${PDFIUM_SKIP_CHECKSUM:-}" ]; then
  echo "OSTRZEŻENIE: PDFIUM_SKIP_CHECKSUM=1 — pomijam weryfikację SHA256 (tryb dev)" >&2
elif [ "$PDFIUM_RELEASE" != "$PINNED_RELEASE" ]; then
  echo "OSTRZEŻENIE: PDFIUM_RELEASE=$PDFIUM_RELEASE != pinowane $PINNED_RELEASE —" >&2
  echo "            known-good SHA256 dotyczy tylko pinowanego release; pomijam" >&2
  echo "            weryfikację. Zaktualizuj sumy w skrypcie po zmianie pinu." >&2
else
  ACTUAL_SHA="$(sha256_of "$ARCHIVE")"
  if [ "$ACTUAL_SHA" != "$SHA256" ]; then
    echo "BLAD: SHA256 nie pasuje dla $ASSET ($PDFIUM_RELEASE)!" >&2
    echo "      oczekiwane: $SHA256" >&2
    echo "      otrzymane:  $ACTUAL_SHA" >&2
    rm -f "$ARCHIVE"
    exit 1
  fi
  echo ">>> SHA256 OK: $ACTUAL_SHA"
fi

# Bug 4: hartowanie ekstrakcji. Najpierw odrzuć wpisy z path-traversal
# (ścieżki absolutne lub zawierające `..`), żeby złośliwe archiwum nie zapisało
# poza $WORK (a tym bardziej poza native-libs). Dopiero potem rozpakuj do
# izolowanego podkatalogu i kopiujemy WYŁĄCZNIE oczekiwane pliki.
EXTRACT="$WORK/extract"
reset_dir "$EXTRACT"
if tar -tzf "$ARCHIVE" | grep -Eq '(^/|^\.\./|/\.\./|/\.\.$|^\.\.$)'; then
  echo "BLAD: archiwum pdfium zawiera ścieżki path-traversal — odrzucam" >&2
  tar -tzf "$ARCHIVE" | grep -E '(^/|\.\.)' >&2 || true
  exit 1
fi
tar -xzf "$ARCHIVE" -C "$EXTRACT"

# bblanchon układa: bin/<lib> (lub lib/<lib>), include/, LICENSE.
SRC_LIB=""
for cand in "$EXTRACT/lib/$LIBNAME" "$EXTRACT/bin/$LIBNAME"; do
  if [ -f "$cand" ]; then SRC_LIB="$cand"; break; fi
done
if [ -z "$SRC_LIB" ]; then
  echo "BLAD: nie znalazłem $LIBNAME w rozpakowanym archiwum $EXTRACT" >&2
  find "$EXTRACT" -maxdepth 2 -type f >&2
  exit 1
fi

DYNAMIC_DIR="$NATIVE_ROOT/$PLATFORM/lib-dynamic"
mkdir -p "$DYNAMIC_DIR"
cp -f "$SRC_LIB" "$DYNAMIC_DIR/$LIBNAME"

# Nagłówki (opcjonalne — wrapper pdfium-render nie linkuje statycznie, ale
# trzymamy je dla spójności layoutu native-libs).
if [ -d "$EXTRACT/include" ]; then
  mkdir -p "$NATIVE_ROOT/$PLATFORM/include/pdfium"
  cp -rf "$EXTRACT/include/." "$NATIVE_ROOT/$PLATFORM/include/pdfium/"
fi

# Licencje (PDFium BSD-3 + bblanchon MIT) obok biblioteki — wymóg dystrybucji.
for lic in LICENSE LICENSE.txt; do
  if [ -f "$EXTRACT/$lic" ]; then
    cp -f "$EXTRACT/$lic" "$DYNAMIC_DIR/LICENSE.pdfium"
    break
  fi
done

append_manifest_library "$PLATFORM" "pdfium" "dynamic" "$PDFIUM_RELEASE" \
  "Prebuilt Google PDFium (non-v8) z bblanchon/pdfium-binaries; ładowany runtime'em przez bind_to_library."

echo ">>> pdfium gotowy: $DYNAMIC_DIR/$LIBNAME"

# Sanity-check izolacji symboli: prebuilt eksportuje wyłącznie FPDF_*; nie może
# wnosić ggml_*/onnx (kolizja z innymi vendorami). Best-effort (nm może nie być).
if command -v nm >/dev/null 2>&1 && [ "${LIBNAME##*.}" = "so" ]; then
  FPDF_CNT="$(nm -D --defined-only "$DYNAMIC_DIR/$LIBNAME" 2>/dev/null | grep -c 'FPDF' || true)"
  GGML_CNT="$(nm -D --defined-only "$DYNAMIC_DIR/$LIBNAME" 2>/dev/null | grep -ic 'ggml_' || true)"
  echo ">>> nm: FPDF_*=$FPDF_CNT, ggml_*=$GGML_CNT (oczekiwane: FPDF>0, ggml=0)"
  if [ "${FPDF_CNT:-0}" -eq 0 ]; then
    echo "OSTRZEŻENIE: brak symboli FPDF_* — czy to na pewno libpdfium?" >&2
  fi
  if [ "${GGML_CNT:-0}" -ne 0 ]; then
    echo "BLAD: libpdfium eksportuje symbole ggml_* — kolizja izolacji!" >&2
    exit 1
  fi
fi
