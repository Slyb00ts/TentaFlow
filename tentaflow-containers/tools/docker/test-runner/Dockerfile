# =============================================================================
# File: Dockerfile — isolated test-runner image (pytest/Playwright/Locust).
# Example (context = project root):
#   docker build -f tentaflow-containers/tools/docker/test-runner/Dockerfile -t tentaflow-test-runner .
# =============================================================================

FROM mcr.microsoft.com/playwright/python:v1.60.0-noble

WORKDIR /app

COPY tentaflow-containers/tools/docker/test-runner/requirements.lock /app/requirements.lock
RUN pip install --no-cache-dir --upgrade pip \
    && pip install --no-cache-dir -r /app/requirements.lock

COPY tentaflow-containers/tools/docker/test-runner/server.py /app/server.py
COPY tentaflow-containers/tools/docker/test-runner/executor /app/executor

# TEST_RUNNER_ISOLATED=1 marks this deployment as the sandboxed variant:
# /health reports isolated:true and build_profile items are allowed.
ENV PORT=8093 \
    TEST_RUNNER_ISOLATED=1 \
    TEST_RUNNER_SKIP_BROWSER_INSTALL=1 \
    TEST_RUNNER_WORK_DIR=/var/lib/tentaflow/test-runner/runs

RUN mkdir -p /var/lib/tentaflow/test-runner/runs \
    && chown -R pwuser:pwuser /app /var/lib/tentaflow

USER pwuser

EXPOSE 8093

CMD ["sh", "-c", "uvicorn --app-dir /app server:app --host 0.0.0.0 --port ${PORT:-8093}"]
