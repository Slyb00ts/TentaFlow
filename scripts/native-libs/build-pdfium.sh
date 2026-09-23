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

# Release tag bblanchon/pdfium-binaries (`chromium/<n>` = gałąź Chromium) i
# sumy SHA-256 assetów pochodzą z scripts/versions.env. bblanchon NIE publikuje
# sidecarów .sha256, więc sumy są pinem integralności prebuiltu.
PDFIUM_RELEASE="$(require_version PDFIUM_RELEASE)"
case "$PLATFORM" in
  linux-x86_64)   ASSET="pdfium-linux-x64.tgz";        LIBNAME="libpdfium.so" ;;
  linux-aarch64)  ASSET="pdfium-linux-arm64.tgz";      LIBNAME="libpdfium.so" ;;
  macos-x86_64)   ASSET="pdfium-mac-x64.tgz";          LIBNAME="libpdfium.dylib" ;;
  macos-arm64)    ASSET="pdfium-mac-arm64.tgz";        LIBNAME="libpdfium.dylib" ;;
  windows-x86_64) ASSET="pdfium-win-x64.tgz";          LIBNAME="pdfium.dll" ;;
  android-arm64)  ASSET="pdfium-android-arm64.tgz";    LIBNAME="libpdfium.so" ;;
  android-armv7)  ASSET="pdfium-android-arm.tgz";      LIBNAME="libpdfium.so" ;;
  android-x86_64) ASSET="pdfium-android-x64.tgz";      LIBNAME="libpdfium.so" ;;
  ios-arm64)      ASSET="pdfium-ios-device-arm64.tgz"; LIBNAME="libpdfium.dylib" ;;
  *)
    echo "Nieobsługiwana platforma pdfium: $PLATFORM" >&2
    exit 1
    ;;
esac
SHA256_KEY="PDFIUM_SHA256_$(printf '%s' "$PLATFORM" | tr 'a-z-' 'A-Z_')"
SHA256="$(pinned_checksum "$SHA256_KEY" PDFIUM_RELEASE)"

BASE_URL="https://github.com/bblanchon/pdfium-binaries/releases/download/${PDFIUM_RELEASE}"
URL="${BASE_URL}/${ASSET}"

WORK="$NATIVE_CACHE/build/pdfium/$PLATFORM"
reset_dir "$WORK"
ARCHIVE="$WORK/$ASSET"

echo ">>> Pobieram prebuilt pdfium: $URL"
require_cmd curl tar
curl -fsSL "$URL" -o "$ARCHIVE"

# Weryfikacja SHA256 prebuiltu PRZED ekstrakcją — bez tego MITM/zatruty cache
# mógłby podmienić bibliotekę ładowaną runtime'em. Nie ma trybu bez sumy:
# inna wersja wymaga jej sumy (pinned_checksum).
ACTUAL_SHA="$(sha256_of "$ARCHIVE")"
if [ "$ACTUAL_SHA" != "$SHA256" ]; then
  echo "BLAD: SHA256 nie pasuje dla $ASSET ($PDFIUM_RELEASE)!" >&2
  echo "      oczekiwane: $SHA256" >&2
  echo "      otrzymane:  $ACTUAL_SHA" >&2
  rm -f "$ARCHIVE"
  exit 1
fi
echo ">>> SHA256 OK: $ACTUAL_SHA"

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
