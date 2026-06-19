# =============================================================================
# Plik: Dockerfile
# Opis: Kontener LLM z Ollama, direct-http (bez sidecara).
# =============================================================================

FROM ollama/ollama:latest
RUN apt-get update && apt-get install -y --no-install-recommends \
      curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY tentaflow-containers/llm/docker/ollama/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

HEALTHCHECK --interval=30s --timeout=5s --start-period=60s --retries=3 \
    CMD curl -fsS "http://127.0.0.1:${OLLAMA_PORT:-11434}/api/tags" || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
