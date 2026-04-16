#!/bin/bash
# =============================================================================
# Plik: build-containers.sh
# Opis: Buduje obrazy Docker kontenerow TentaFlow i opcjonalnie pushuje do
#       registry. Iteruje katalogi <kategoria>/docker/<engine>/ z plikiem
#       build.sh.
# =============================================================================
#
# Uzycie:
#   ./build-containers.sh                              # wszystkie kontenery
#   ./build-containers.sh teams-bot                    # tylko teams-bot
#   ./build-containers.sh --category llm               # cala kategoria llm
#   ./build-containers.sh --push                       # buduj i pushuj
#   ./build-containers.sh --push teams-bot             # buduj i pushuj teams-bot
#   ./build-containers.sh --full                       # pelny rebuild bez cache
#   ./build-containers.sh --list                       # lista dostepnych
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
FILTER_CATEGORY=""

# Funkcja: zwraca wszystkie kontenery jako "<kategoria>/<engine>" jesli maja build.sh
discover_containers() {
    local results=()
    for category_dir in "$SCRIPT_DIR"/*/; do
        local category="$(basename "$category_dir")"
        local docker_dir="$category_dir/docker"
        [ -d "$docker_dir" ] || continue
        for engine_dir in "$docker_dir"/*/; do
            [ -d "$engine_dir" ] || continue
            local engine="$(basename "$engine_dir")"
            if [ -f "$engine_dir/build.sh" ]; then
                results+=("$category/$engine")
            fi
        done
    done
    printf "%s\n" "${results[@]}"
}

# Funkcja: znajduje kontener po samej nazwie engine (np. "teams-bot" -> "agents/teams-bot")
resolve_engine_name() {
    local name="$1"
    # Jesli juz w formacie kategoria/engine
    if [[ "$name" == */* ]]; then
        echo "$name"
        return
    fi
    while IFS= read -r entry; do
        if [[ "${entry##*/}" == "$name" ]]; then
            echo "$entry"
            return
        fi
    done < <(discover_containers)
    # Nie znaleziono — zwroc puste
    echo ""
}

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
        --category)
            FILTER_CATEGORY="$2"
            shift 2
            ;;
        --list)
            echo -e "${BLUE}Dostepne kontenery:${NC}"
            while IFS= read -r entry; do
                echo -e "  ${GREEN}${entry}${NC}"
            done < <(discover_containers)
            exit 0
            ;;
        --help|-h)
            echo "Uzycie: $0 [opcje] [kontenery...]"
            echo ""
            echo "Opcje:"
            echo "  --push              Pushuj do registry po zbudowaniu"
            echo "  --full              Pelny rebuild bez cache"
            echo "  --category <name>   Buduj tylko kontenery z danej kategorii"
            echo "  --list              Lista dostepnych kontenerow"
            echo ""
            echo "Zmienne:"
            echo "  REGISTRY  Registry docelowe (domyslnie: ghcr.io/slyb00ts)"
            echo "  TAG       Tag obrazu (domyslnie: latest)"
            echo ""
            echo "Bez argumentow buduje wszystkie kontenery."
            echo "Kontener mozna podac jako 'kategoria/engine' (np. llm/vllm) albo"
            echo "samym 'engine' (np. teams-bot) — skrypt rozwiaze kategorie."
            exit 0
            ;;
        *)
            REQUESTED+=("$1")
            shift
            ;;
    esac
done

# Jesli nie podano kontenerow - zbierz wszystkie
if [ ${#REQUESTED[@]} -eq 0 ]; then
    while IFS= read -r entry; do
        REQUESTED+=("$entry")
    done < <(discover_containers)
fi

# Filtr po kategorii (jesli ustawiony)
if [ -n "$FILTER_CATEGORY" ]; then
    FILTERED=()
    for entry in "${REQUESTED[@]}"; do
        resolved="$(resolve_engine_name "$entry")"
        [ -z "$resolved" ] && resolved="$entry"
        if [[ "$resolved" == "${FILTER_CATEGORY}/"* ]]; then
            FILTERED+=("$resolved")
        fi
    done
    REQUESTED=("${FILTERED[@]}")
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
[ -n "$FILTER_CATEGORY" ] && echo -e "Kategoria:  ${GREEN}${FILTER_CATEGORY}${NC}"
echo -e "Kontenery:  ${GREEN}${REQUESTED[*]}${NC}"
echo ""

FAILED=()
SUCCESS=()

for name in "${REQUESTED[@]}"; do
    resolved="$(resolve_engine_name "$name")"
    if [ -z "$resolved" ]; then
        echo -e "${RED}Nieznany kontener: $name (nie znaleziono w zadnej kategorii)${NC}"
        FAILED+=("$name")
        continue
    fi

    CONTAINER_DIR="$SCRIPT_DIR/${resolved%/*}/docker/${resolved##*/}"
    BUILD_SCRIPT="$CONTAINER_DIR/build.sh"

    if [ ! -d "$CONTAINER_DIR" ]; then
        echo -e "${RED}Nieznany kontener: $resolved (brak katalogu)${NC}"
        FAILED+=("$resolved")
        continue
    fi

    if [ ! -f "$BUILD_SCRIPT" ]; then
        echo -e "${RED}Brak build.sh w: $resolved${NC}"
        FAILED+=("$resolved")
        continue
    fi

    echo -e "${BLUE}────────────────────────────────────────────────────────────${NC}"
    echo -e "${YELLOW}Budowanie: ${NC}${GREEN}${resolved}${NC}"
    echo -e "${BLUE}────────────────────────────────────────────────────────────${NC}"

    START_TIME=$(date +%s)

    if REGISTRY="$REGISTRY" TAG="$TAG" BUILD_OPTS="$BUILD_OPTS" \
       DO_PUSH="$DO_PUSH" PROJECT_ROOT="$PROJECT_ROOT" \
       bash "$BUILD_SCRIPT"; then
        END_TIME=$(date +%s)
        DURATION=$((END_TIME - START_TIME))
        echo -e "${GREEN}OK: ${resolved}${NC} (${DURATION}s)"
        SUCCESS+=("$resolved")
    else
        END_TIME=$(date +%s)
        DURATION=$((END_TIME - START_TIME))
        echo -e "${RED}BLAD: ${resolved}${NC} (${DURATION}s)"
        FAILED+=("$resolved")
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
