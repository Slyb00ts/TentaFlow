# =============================================================================
# Plik: Dockerfile
# Opis: Kontener detektorow dokumentowych YOLOX (NVIDIA NeMo Retriever) z torch
#       cu130 (PyTorch GPU), direct-http (bez sidecara). Wspolny obraz dla trzech
#       modeli (page-elements, graphic-elements, table-structure) - konkretny model
#       wybiera zmienna MODEL_REPO. Serwer laduje wagi .pth do sieci YOLOX i liczy
#       inferencje BEZPOSREDNIO w PyTorch na GPU (custom API /detect). onnxruntime
#       NIE jest instalowany — B300 (sm_103) nie ma kerneli onnxruntime-gpu,
#       a torch cu130 wspiera sm_103.
# Przykład: docker build -f vision/docker/nemotron-yolox/Dockerfile .
# =============================================================================

FROM nvidia/cuda:13.0.3-cudnn-runtime-ubuntu24.04
RUN apt-get update && apt-get install -y --no-install-recommends \
      python3 python3-pip python3-venv python3-dev git curl ca-certificates \
      build-essential ninja-build \
      libgl1 libglib2.0-0 \
    && rm -rf /var/lib/apt/lists/*

RUN python3 -m venv /opt/venv
ENV PATH="/opt/venv/bin:$PATH"
# torch cu130 serwuje inferencje YOLOX bezposrednio na GPU (wspiera sm_103/B300).
# YOLOX dostarcza tylko definicje sieci do zaladowania wag .pth — server.py
# importuje wylacznie `yolox.exp`. Instalujemy go z `--no-deps`, zeby pominac
# ciezkie zaleznosci treningowe (pycocotools/onnx/onnx-simplifier/tensorboard),
# ktorych inferencja nie potrzebuje, a ktore i tak wymagaja kompilacji. Z importu
# `yolox.exp` na top-levelu wynikaja tylko loguru + psutil + packaging + tabulate.
# `--no-build-isolation`, bo setup.py YOLOX na Linuksie wymusza kompilacje op
# FastCOCOEvalOp (CppExtension) i `assert TORCH_AVAILABLE` — w izolacji PEP 517
# torch jest niewidoczny i build pada; tu uzywamy torcha juz w /opt/venv, a
# build-essential + ninja dostarczaja g++ do kompilacji op. Bez onnxruntime.
RUN pip install --no-cache-dir --upgrade pip wheel setuptools \
 && pip install --no-cache-dir --index-url https://download.pytorch.org/whl/cu130 \
      torch torchvision \
 && pip install --no-cache-dir \
      fastapi uvicorn python-multipart \
      opencv-python-headless pillow numpy \
      huggingface_hub loguru psutil packaging tabulate \
 && pip install --no-cache-dir --no-build-isolation --no-deps \
      "yolox @ git+https://github.com/Megvii-BaseDetection/YOLOX.git"

COPY tentaflow-containers/vision/python/nemotron-yolox/server.py /app/server.py
COPY tentaflow-containers/vision/python/nemotron-yolox/export_onnx.py /app/export_onnx.py
COPY tentaflow-containers/vision/docker/nemotron-yolox/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

ENV MODEL_DIR=/data/models \
    NEMOTRON_YOLOX_PORT=8086

HEALTHCHECK --interval=30s --timeout=5s --start-period=180s --retries=3 \
    CMD curl -fsS "http://127.0.0.1:${PORT:-8086}/health" || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
