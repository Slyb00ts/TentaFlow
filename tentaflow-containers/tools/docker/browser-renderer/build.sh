#!/bin/bash
# =============================================================================
# Plik: build.sh
# Opis: Buduje obraz Docker browser-renderer dla TentaFlow.
# Przykład: ./build.sh ghcr.io/org/browser-renderer latest
# =============================================================================

set -e

REGISTRY="${1:-ghcr.io/slyb00ts}"
TAG="${2:-latest}"
IMAGE="${REGISTRY}/tentaflow-browser-renderer:${TAG}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="${PROJECT_ROOT:-$(cd "$SCRIPT_DIR/../../../.." && pwd)}"

# Kontekstem jest root projektu — Dockerfile COPY-uje pliki przez pelna sciezke
# tentaflow-containers/... (spojnie z deployem przez core).
docker build -t "${IMAGE}" -f "$SCRIPT_DIR/Dockerfile" "$PROJECT_ROOT"
echo "Zbudowano ${IMAGE}"
