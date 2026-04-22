#!/bin/bash
# =============================================================================
# Plik: tentaflow-mobile/ios/scripts/fix-clang-version.sh
# Opis: Wykrywa biezaca wersje clang w aktywnym Xcode i synchronizuje hardcoded
#       sciezki w project.pbxproj (LIBRARY_SEARCH_PATHS + OTHER_LDFLAGS wskazujace
#       na libclang_rt.ios.a). Uruchamiaj po kazdym major upgrade Xcode,
#       albo pozwol build-rust.sh wolac to automatycznie.
# =============================================================================
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PBXPROJ="$SCRIPT_DIR/../TentaFlowAI.xcodeproj/project.pbxproj"

if [ ! -f "$PBXPROJ" ]; then
    echo "ERROR: nie znaleziono $PBXPROJ"
    exit 1
fi

CLANG_RT_DIR=$(dirname "$(xcrun --toolchain default -f clang)")/../lib/clang

# Preferuj krotka, czysto numeryczna nazwe (np. "21") — to tam faktycznie siedza
# pliki; warianty typu "21.0.0" to symlinki, ktore moga zniknac przy upgrade.
CLANG_VERSION=$(ls "$CLANG_RT_DIR" 2>/dev/null | grep -E '^[0-9]+$' | sort -V | tail -1)

# Fallback: jesli z jakiegos powodu brak czysto numerycznej, wez cokolwiek.
if [ -z "$CLANG_VERSION" ]; then
    CLANG_VERSION=$(ls "$CLANG_RT_DIR" 2>/dev/null | sort -V | tail -1)
fi

if [ -z "$CLANG_VERSION" ]; then
    echo "ERROR: nie znaleziono zadnej wersji clang w $CLANG_RT_DIR"
    exit 1
fi

# Sprawdz czy trzeba cokolwiek zmieniac (idempotent).
# Regex musi lapac zarowno "21" jak i "21.0.0" — oba sa legalne nazwy katalogow.
CURRENT=$(grep -oE 'clang/[0-9][0-9.]*/lib/darwin' "$PBXPROJ" | head -1 | cut -d/ -f2)

if [ "$CURRENT" = "$CLANG_VERSION" ]; then
    echo "pbxproj juz uzywa clang/$CLANG_VERSION — nic do zrobienia"
    exit 0
fi

echo "Aktualizacja pbxproj: clang/$CURRENT -> clang/$CLANG_VERSION"

# sed -i '' jest wymagany na BSD sed (macOS).
sed -i '' -E "s|(clang/)[0-9][0-9.]*(/lib/darwin)|\\1${CLANG_VERSION}\\2|g" "$PBXPROJ"

echo "Gotowe. Plik: $PBXPROJ"
