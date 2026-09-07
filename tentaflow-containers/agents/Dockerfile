# =============================================================================
# Plik: Dockerfile
# Opis: Buduje bridge agentów z eksportowanego katalogu zależności workspace.
# =============================================================================
FROM rust:1-bookworm AS bridge-build
RUN apt-get update && apt-get install -y --no-install-recommends python3 && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY vendor/ vendor/
COPY scripts/export-workspace.py scripts/export-workspace.py
COPY tentaflow-containers/agents/native/ tentaflow-containers/agents/native/
RUN python3 scripts/export-workspace.py --root . --output . --in-place --allow-network --member tentaflow-containers/agents/native/coding-agent-bridge \
    && cargo build --release --locked --package tentaflow-coding-agent-bridge

FROM node:22-bookworm-slim
ARG CLI_PACKAGE
ARG CLI_VERSION
RUN test -n "$CLI_PACKAGE" && test -n "$CLI_VERSION" \
    && apt-get update \
    && apt-get install -y --no-install-recommends git ca-certificates bash \
    && rm -rf /var/lib/apt/lists/* \
    && npm install --global "${CLI_PACKAGE}@${CLI_VERSION}"
COPY --from=bridge-build /src/target/release/tentaflow-coding-agent-bridge /usr/local/bin/tentaflow-coding-agent-bridge
RUN mkdir -p /data /workspace
ENV TENTAFLOW_CODING_AGENT_DATA_DIR=/data
ENV TENTAFLOW_WORKSPACE_ROOT=/workspace
ENV TENTAFLOW_BIND_HOST=0.0.0.0
ENV TENTAFLOW_AGENT_EXECUTION=container
ENV CODEX_HOME=/data/codex
ENV CLAUDE_CONFIG_DIR=/data/claude
ENV HOME=/data/home
VOLUME ["/data"]
WORKDIR /workspace
EXPOSE 8765
ENTRYPOINT ["tentaflow-coding-agent-bridge"]
