#!/usr/bin/env bash
# =============================================================================
# Plik: scripts/cleanup-targets.sh
# Opis: Usuwa per-crate `target/` foldery z kazdego sub-crate'a w repo. Po
#       ustawieniu `target-dir = "target_shared"` w .cargo/config.toml caly
#       build idzie do jednego wspoldzielonego katalogu — stare per-crate
#       target/ to czysty waste (typowo ~200 GB lacznie).
#       Runtime data zyje teraz w `.runtime/`, wiec usuniecie target/ jest
#       bezpieczne dla bazy danych i kluczy HMAC.
#
# Uzycie: ./scripts/cleanup-targets.sh        # interaktywne podsumowanie
#         ./scripts/cleanup-targets.sh --yes  # bez potwierdzenia
# =============================================================================
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

echo "Skanuje per-crate target/ foldery pod $REPO_ROOT ..."
mapfile -t TARGETS < <(find . -maxdepth 3 -name target -type d \
    -not -path './target_shared*' \
    -not -path './.runtime/*' \
    -not -path './thirdparty/*' \
    -not -path './tentaflow-containers/*' \
    -prune)

if [[ ${#TARGETS[@]} -eq 0 ]]; then
    echo "Brak per-crate target/ folderow — repo juz uzywa wylacznie target_shared/."
    exit 0
fi

TOTAL_BYTES=0
for t in "${TARGETS[@]}"; do
    size_human=$(du -sh "$t" 2>/dev/null | awk '{print $1}')
    size_bytes=$(du -sb "$t" 2>/dev/null | awk '{print $1}')
    TOTAL_BYTES=$((TOTAL_BYTES + size_bytes))
    printf "  %8s  %s\n" "$size_human" "$t"
done
total_human=$(numfmt --to=iec --suffix=B "$TOTAL_BYTES")
echo "Do zwolnienia: $total_human"

if [[ "${1:-}" != "--yes" ]]; then
    read -r -p "Usunac wszystkie powyzsze target/? [y/N] " ans
    case "$ans" in
        y|Y|yes|YES) ;;
        *) echo "Anulowane."; exit 0 ;;
    esac
fi

for t in "${TARGETS[@]}"; do
    rm -rf "$t"
    echo "Usunieto $t"
done

echo
echo "Gotowe. Kolejny build pojdzie do $REPO_ROOT/target_shared/."
echo "Opcjonalnie: zainstaluj sccache zeby przyspieszyc clean buildy:"
echo "    cargo install sccache && export RUSTC_WRAPPER=sccache"
