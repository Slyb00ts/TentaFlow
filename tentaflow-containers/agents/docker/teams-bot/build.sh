#!/bin/bash
# =============================================================================
# Plik: build.sh
# Opis: Buduje obraz Docker kontenera meeting sidecar (teams-bot).
#       Wywoływany przez build-containers.sh lub recznie.
# =============================================================================

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="${PROJECT_ROOT:-$(cd "$SCRIPT_DIR/../../../.." && pwd)}"

REGISTRY="${REGISTRY:-ghcr.io/slyb00ts}"
TAG="${TAG:-latest}"
BUILD_OPTS="${BUILD_OPTS:-}"
DO_PUSH="${DO_PUSH:-false}"

IMAGE_NAME="tentaflow-meeting-sidecar"
FULL_IMAGE="${REGISTRY}/${IMAGE_NAME}:${TAG}"

echo "Obraz: ${FULL_IMAGE}"
echo "Kontekst: ${PROJECT_ROOT}"

# Budowanie — kontekstem jest root projektu (potrzebny dostep do crate'ow)
docker build $BUILD_OPTS \
    -t "$FULL_IMAGE" \
    -f "$SCRIPT_DIR/Dockerfile" \
    "$PROJECT_ROOT"

if [ "$DO_PUSH" = true ]; then
    echo "Push: ${FULL_IMAGE}"
    docker push "$FULL_IMAGE"
fi
