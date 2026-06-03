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

docker build -t "${IMAGE}" "$(dirname "$0")"
echo "Zbudowano ${IMAGE}"
