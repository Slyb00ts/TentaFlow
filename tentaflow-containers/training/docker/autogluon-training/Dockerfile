# =============================================================================
# Plik: Dockerfile
# Opis: Kontener tabularnego serwera treningowego AutoGluon (ensemble drzew:
#       LightGBM/CatBoost/XGBoost). Tylko CPU. Serwer FastAPI nasłuchuje na :8201.
# =============================================================================

FROM python:3.11-slim

# libgomp1 jest wymagane przez LightGBM/XGBoost (OpenMP) w obrazie slim.
RUN apt-get update \
    && apt-get install -y --no-install-recommends libgomp1 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# requirements.lock to to samo źródło prawdy co native bundle
# (training/python/autogluon-training/requirements.lock) — trzymane jako kopia,
# bo build context Dockera nie sięga poza swój katalog.
COPY requirements.lock /app/requirements.lock
RUN pip install --no-cache-dir -r /app/requirements.lock

# server.py to ta sama logika co native bundle
# (training/python/autogluon-training/server.py) — identyczna kopia w obu
# lokalizacjach z tego samego powodu (ograniczony build context).
COPY server.py /app/server.py

EXPOSE 8201

HEALTHCHECK --interval=30s --timeout=5s --start-period=60s --retries=3 \
    CMD python -c "import urllib.request; urllib.request.urlopen('http://127.0.0.1:8201/health')" || exit 1

CMD ["uvicorn", "server:app", "--host", "0.0.0.0", "--port", "8201"]
