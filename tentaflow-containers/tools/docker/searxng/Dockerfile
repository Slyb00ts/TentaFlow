# =============================================================================
# Plik: Dockerfile
# Opis: Obraz SearXNG dla TentaFlow z wlaczonym formatem JSON dla API search.
# Przykład (kontekst = root projektu):
#   docker build -f tentaflow-containers/tools/docker/searxng/Dockerfile -t tentaflow-searxng .
# =============================================================================

FROM searxng/searxng:2026.5.17-d7e8b7cd1

COPY tentaflow-containers/tools/docker/searxng/settings.yml /etc/searxng/settings.yml
COPY tentaflow-containers/tools/docker/searxng/entrypoint.sh /usr/local/bin/tentaflow-searxng-entrypoint

EXPOSE 8080

ENTRYPOINT ["/usr/local/bin/tentaflow-searxng-entrypoint"]
