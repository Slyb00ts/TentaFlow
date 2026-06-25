# =============================================================================
# Plik: Dockerfile
# Opis: Kontener Nemotron-OCR (CUDA-only), direct-http (bez sidecara). server.py
#       nasluchuje HTTP na 0.0.0.0:8093, Core gada HTTP wprost.
# =============================================================================

# --- Nemotron-OCR na CUDA ---
# Wariant devel (nie runtime) — pakiet nemotron-ocr buduje rozszerzenie CUDA
# (torch CUDAExtension), wiec nvcc/toolkit CUDA musi byc dostepny przy instalacji.
FROM nvidia/cuda:13.0.3-cudnn-devel-ubuntu24.04
RUN apt-get update && apt-get install -y --no-install-recommends \
      python3 python3-pip python3-venv python3-dev curl ca-certificates \
      git git-lfs build-essential ninja-build \
    && rm -rf /var/lib/apt/lists/*

RUN python3 -m venv /opt/ocr-venv
ENV PATH="/opt/ocr-venv/bin:$PATH"
RUN pip install --no-cache-dir --upgrade pip wheel

# torch+torchvision cu130 dla B300 (sm_103) — domyslny PyPI to wariant CPU/cu129.
# Instalujemy je PRZED pakietem nemotron-ocr, zeby pip uznal torch>=2.8 /
# torchvision>=0.23 za spelnione i NIE nadpisal ich wariantem CPU z PyPI.
# nvidia/nemotron-ocr-v1 to NIE model transformers — to pakiet z wlasnym
# pipeline'em (detector+recognizer+relational) i rozszerzeniem C++ budowanym
# przez hatch_build.py, dlatego instalujemy go ze zrodel HF z --no-build-isolation
# (build widzi juz zainstalowany torch). Wagi .pth pobiera pipeline w runtime.
RUN pip install --no-cache-dir \
      --extra-index-url https://download.pytorch.org/whl/cu130 \
      torch==2.11.0 torchvision==0.26.0
RUN pip install --no-cache-dir \
      fastapi==0.115.6 uvicorn==0.34.0 python-multipart==0.0.20 \
      "huggingface_hub[cli,hf_transfer]" hatchling setuptools
# Rozszerzenie CUDA pakietu nemotron-ocr (quad_non_maximal_suppression przez nvcc)
# kompiluje sie WYLACZNIE pod architektury z TORCH_CUDA_ARCH_LIST. Bez tego nvcc
# bierze domyslna liste torcha, ktora przy `docker build` (brak widocznego GPU)
# NIE obejmuje najnowszego Blackwella -> na **B300 (sm_103, DGX)** leci `CUDA error:
# no kernel image is available for execution on the device` przy /infer, mimo ze
# B300 to docelowy sprzet. Budujemy FAT-BINARY pod wszystkie karty meshu, zeby ten
# sam obraz dzialal wszedzie: Ampere 8.6 (RTX 3090), Ada 8.9 (4090/L40), Hopper 9.0
# (H100), Blackwell 10.0 (B200) i 10.3 (B300) + PTX dla nowszych (forward-compat
# JIT, np. GB10/Spark). Musi byc USTAWIONE PRZED budowa pakietu nizej.
ENV TORCH_CUDA_ARCH_LIST="8.6;8.9;9.0;10.0;10.3+PTX"

# Klonujemy repo z pominieciem LFS (wagi .pth nie sa potrzebne do builda pakietu;
# pipeline pobiera je w runtime). `git lfs install --skip-smudge` rejestruje filtr
# LFS w trybie skip, dzieki czemu checkout dostaje wskazniki zamiast wag i nie
# pada. Pakiet instalujemy z lokalnego podkatalogu (rozszerzenie C++ przez hatch).
RUN git lfs install --skip-smudge \
    && git clone --depth 1 https://huggingface.co/nvidia/nemotron-ocr-v1 /tmp/ocr-src \
    && pip install --no-cache-dir --no-build-isolation /tmp/ocr-src/nemotron-ocr \
    && rm -rf /tmp/ocr-src
ENV HF_HUB_ENABLE_HF_TRANSFER=1

COPY tentaflow-containers/vision/python/nemotron-ocr/server.py /app/server.py
COPY tentaflow-containers/vision/docker/nemotron-ocr/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

HEALTHCHECK --interval=30s --timeout=5s --start-period=300s --retries=3 \
    CMD curl -fsS "http://127.0.0.1:${PORT:-8093}/health" || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
