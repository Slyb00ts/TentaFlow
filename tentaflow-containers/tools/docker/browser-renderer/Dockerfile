# =============================================================================
# Plik: Dockerfile
# Opis: Obraz Playwright Chromium renderera dla stron JS-heavy w web research.
# Przykład (kontekst = root projektu):
#   docker build -f tentaflow-containers/tools/docker/browser-renderer/Dockerfile -t tentaflow-browser-renderer .
# =============================================================================

FROM mcr.microsoft.com/playwright/python:v1.60.0-noble

WORKDIR /app

COPY tentaflow-containers/tools/docker/browser-renderer/requirements.lock /app/requirements.lock
RUN pip install --no-cache-dir --upgrade pip \
    && pip install --no-cache-dir -r /app/requirements.lock

COPY tentaflow-containers/tools/docker/browser-renderer/server.py /app/server.py

ENV PORT=8092 \
    BROWSER_RENDERER_SKIP_BROWSER_INSTALL=1 \
    BROWSER_RENDERER_PROFILE_DIR=/var/lib/tentaflow/browser-renderer/profiles

RUN mkdir -p /var/lib/tentaflow/browser-renderer/profiles \
    && chown -R pwuser:pwuser /app /var/lib/tentaflow

USER pwuser

EXPOSE 8092

CMD ["sh", "-c", "uvicorn --app-dir /app server:app --host 0.0.0.0 --port ${PORT:-8092}"]
