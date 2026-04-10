#!/bin/bash
# =============================================================================
# Plik: build-containers.sh
# Opis: Buduje obrazy Docker kontenerów TentaFlow i opcjonalnie pushuje do
#       registry. Iteruje podkatalogi z plikiem build.sh.
# =============================================================================
#
# Uzycie:
#   ./build-containers.sh                    # buduje wszystkie kontenery
#   ./build-containers.sh teams-bot          # tylko teams-bot
#   ./build-containers.sh --push             # buduj i pushuj do registry
#   ./build-containers.sh --push teams-bot   # buduj i pushuj teams-bot
#   ./build-containers.sh --full             # pelny rebuild bez cache
#   ./build-containers.sh --list             # lista dostepnych kontenerow
#
# Zmienne srodowiskowe:
#   REGISTRY  - registry docelowe (domyslnie: ghcr.io/slyb00ts)
#   TAG       - tag obrazu (domyslnie: latest)
#
# =============================================================================

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

REGISTRY="${REGISTRY:-ghcr.io/slyb00ts}"
TAG="${TAG:-latest}"

# Kolory
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

# Opcje
DO_PUSH=false
BUILD_OPTS=""
REQUESTED=()

# Parsowanie argumentow
while [[ $# -gt 0 ]]; do
    case $1 in
        --push)
            DO_PUSH=true
            shift
            ;;
        --full)
            BUILD_OPTS="--no-cache"
            shift
            ;;
        --list)
            echo -e "${BLUE}Dostepne kontenery:${NC}"
            for dir in "$SCRIPT_DIR"/*/; do
                name="$(basename "$dir")"
                if [ -f "$dir/build.sh" ]; then
                    echo -e "  ${GREEN}${name}${NC}"
                fi
            done
            exit 0
            ;;
        --help|-h)
            echo "Uzycie: $0 [opcje] [kontenery...]"
            echo ""
            echo "Opcje:"
            echo "  --push    Pushuj do registry po zbudowaniu"
            echo "  --full    Pelny rebuild bez cache"
            echo "  --list    Lista dostepnych kontenerow"
            echo ""
            echo "Zmienne:"
            echo "  REGISTRY  Registry docelowe (domyslnie: ghcr.io/slyb00ts)"
            echo "  TAG       Tag obrazu (domyslnie: latest)"
            echo ""
            echo "Bez argumentow buduje wszystkie kontenery."
            exit 0
            ;;
        *)
            REQUESTED+=("$1")
            shift
            ;;
    esac
done

# Jesli nie podano kontenerow - znajdz wszystkie z build.sh
if [ ${#REQUESTED[@]} -eq 0 ]; then
    for dir in "$SCRIPT_DIR"/*/; do
        name="$(basename "$dir")"
        if [ -f "$dir/build.sh" ]; then
            REQUESTED+=("$name")
        fi
    done
fi

# Sprawdz czy cos jest do zbudowania
if [ ${#REQUESTED[@]} -eq 0 ]; then
    echo -e "${YELLOW}Brak kontenerow do zbudowania.${NC}"
    exit 0
fi

echo -e "${BLUE}══════════════════════════════════════════════════════════════${NC}"
echo -e "${BLUE}  TENTAFLOW - Build Containers${NC}"
echo -e "${BLUE}══════════════════════════════════════════════════════════════${NC}"
echo ""
echo -e "Registry:   ${GREEN}${REGISTRY}${NC}"
echo -e "Tag:        ${GREEN}${TAG}${NC}"
echo -e "Push:       $([ "$DO_PUSH" = true ] && echo "${GREEN}TAK${NC}" || echo "${YELLOW}NIE${NC}")"
echo -e "Cache:      $([ -z "$BUILD_OPTS" ] && echo "${GREEN}INKREMENTALNY${NC}" || echo "${YELLOW}PELNY REBUILD${NC}")"
echo -e "Kontenery:  ${GREEN}${REQUESTED[*]}${NC}"
echo ""

FAILED=()
SUCCESS=()

for name in "${REQUESTED[@]}"; do
    CONTAINER_DIR="$SCRIPT_DIR/$name"
    BUILD_SCRIPT="$CONTAINER_DIR/build.sh"

    if [ ! -d "$CONTAINER_DIR" ]; then
        echo -e "${RED}Nieznany kontener: $name (brak katalogu)${NC}"
        FAILED+=("$name")
        continue
    fi

    if [ ! -f "$BUILD_SCRIPT" ]; then
        echo -e "${RED}Brak build.sh w: $name${NC}"
        FAILED+=("$name")
        continue
    fi

    echo -e "${BLUE}────────────────────────────────────────────────────────────${NC}"
    echo -e "${YELLOW}Budowanie: ${NC}${GREEN}${name}${NC}"
    echo -e "${BLUE}────────────────────────────────────────────────────────────${NC}"

    START_TIME=$(date +%s)

    # Wywolaj build.sh kontenera z odpowiednimi zmiennymi
    if REGISTRY="$REGISTRY" TAG="$TAG" BUILD_OPTS="$BUILD_OPTS" \
       DO_PUSH="$DO_PUSH" PROJECT_ROOT="$PROJECT_ROOT" \
       bash "$BUILD_SCRIPT"; then
        END_TIME=$(date +%s)
        DURATION=$((END_TIME - START_TIME))
        echo -e "${GREEN}OK: ${name}${NC} (${DURATION}s)"
        SUCCESS+=("$name")
    else
        END_TIME=$(date +%s)
        DURATION=$((END_TIME - START_TIME))
        echo -e "${RED}BLAD: ${name}${NC} (${DURATION}s)"
        FAILED+=("$name")
    fi

    echo ""
done

# Podsumowanie
echo -e "${BLUE}══════════════════════════════════════════════════════════════${NC}"
echo -e "${BLUE}PODSUMOWANIE${NC}"
echo -e "${BLUE}══════════════════════════════════════════════════════════════${NC}"

if [ ${#SUCCESS[@]} -gt 0 ]; then
    echo -e "${GREEN}OK:     ${SUCCESS[*]}${NC}"
fi

if [ ${#FAILED[@]} -gt 0 ]; then
    echo -e "${RED}BLEDY:  ${FAILED[*]}${NC}"
    exit 1
fi

echo ""
echo -e "${GREEN}Wszystko gotowe!${NC}"
if [ "$DO_PUSH" = true ]; then
    echo -e "Obrazy dostepne w: ${GREEN}${REGISTRY}${NC}"
fi
