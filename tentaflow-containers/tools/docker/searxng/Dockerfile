# =============================================================================
# Plik: Dockerfile
# Opis: Obraz SearXNG dla TentaFlow z wlaczonym formatem JSON dla API search.
# Przykład: docker build -t tentaflow-searxng .
# =============================================================================

FROM searxng/searxng:2026.5.17-d7e8b7cd1

COPY settings.yml /etc/searxng/settings.yml
COPY --chmod=0755 entrypoint.sh /usr/local/bin/tentaflow-searxng-entrypoint

EXPOSE 8080

ENTRYPOINT ["/usr/local/bin/tentaflow-searxng-entrypoint"]
