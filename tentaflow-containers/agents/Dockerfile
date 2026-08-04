FROM rust:1-bookworm AS bridge-build
WORKDIR /src
COPY native/coding-agent-bridge ./native/coding-agent-bridge
RUN cargo build --release --manifest-path native/coding-agent-bridge/Cargo.toml

FROM node:22-bookworm-slim
ARG CLI_PACKAGE
ARG CLI_VERSION
RUN test -n "$CLI_PACKAGE" && test -n "$CLI_VERSION" \
    && apt-get update \
    && apt-get install -y --no-install-recommends git ca-certificates bash \
    && rm -rf /var/lib/apt/lists/* \
    && npm install --global "${CLI_PACKAGE}@${CLI_VERSION}"
COPY --from=bridge-build /src/native/coding-agent-bridge/target/release/tentaflow-coding-agent-bridge /usr/local/bin/tentaflow-coding-agent-bridge
RUN mkdir -p /data /workspace
ENV TENTAFLOW_CODING_AGENT_DATA_DIR=/data
ENV TENTAFLOW_WORKSPACE_ROOT=/workspace
ENV CODEX_HOME=/data/codex
ENV CLAUDE_CONFIG_DIR=/data/claude
ENV HOME=/data
VOLUME ["/data"]
WORKDIR /workspace
EXPOSE 8765
ENTRYPOINT ["tentaflow-coding-agent-bridge"]
