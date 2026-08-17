#!/bin/bash
# =============================================================================
# File: build.sh — builds the TentaFlow test-runner docker image.
# Example: ./build.sh ghcr.io/org/test-runner latest
# =============================================================================

set -e

REGISTRY="${1:-ghcr.io/slyb00ts}"
TAG="${2:-latest}"
IMAGE="${REGISTRY}/tentaflow-test-runner:${TAG}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="${PROJECT_ROOT:-$(cd "$SCRIPT_DIR/../../../.." && pwd)}"

# The context is the project root — the Dockerfile COPY-s files via the full
# tentaflow-containers/... paths (consistent with core-driven deploys).
docker build -t "${IMAGE}" -f "$SCRIPT_DIR/Dockerfile" "$PROJECT_ROOT"
echo "Built ${IMAGE}"
