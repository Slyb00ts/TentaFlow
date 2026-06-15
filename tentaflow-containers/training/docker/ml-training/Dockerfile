# =============================================================================
# Plik: Dockerfile
# Opis: Kontener treningowy LLM (Hugging Face): transformers + peft + trl na
#       PyTorch 2.4.1 / CUDA 12.1. Serwer FastAPI nasłuchuje na :8200.
# =============================================================================

FROM pytorch/pytorch:2.4.1-cuda12.1-cudnn9-runtime

RUN pip install --no-cache-dir \
      transformers==4.46.3 \
      peft==0.13.2 \
      trl==0.12.1 \
      datasets==3.1.0 \
      accelerate==1.1.1 \
      bitsandbytes==0.44.1 \
      fastapi==0.115.5 \
      "uvicorn[standard]==0.32.1" \
      sentencepiece==0.2.0 \
      protobuf==5.28.3

WORKDIR /app

# Build context = ten katalog (context_path = training/docker/ml-training), wiec
# COPY siega tylko lokalnych plikow. server.py to ta sama logika co native bundle
# (training/python/ml-training/server.py) — identyczna kopia trzymana w obu
# lokalizacjach, bo build context Dockera nie siega poza swoj katalog.
COPY server.py /app/server.py

EXPOSE 8200

HEALTHCHECK --interval=30s --timeout=5s --start-period=60s --retries=3 \
    CMD python -c "import urllib.request; urllib.request.urlopen('http://127.0.0.1:8200/health')" || exit 1

CMD ["uvicorn", "server:app", "--host", "0.0.0.0", "--port", "8200"]
