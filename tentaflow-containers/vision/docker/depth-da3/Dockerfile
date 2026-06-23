# =============================================================================
# Plik: Dockerfile
# Opis: Kontener Depth Anything 3 (DA3), direct-http. DA3 ma WŁASNY pakiet
#       (`depth_anything_3` z repo ByteDance-Seed), instalowany dokładnie wg
#       README projektu: `pip install xformers torch>=2 torchvision` + `pip install
#       -e .` z klona repo. Baza CUDA devel (nvcc — pakiet może budować rozszerzenia
#       CUDA). server.py nasłuchuje HTTP na 0.0.0.0:${PORT}.
# =============================================================================

FROM nvidia/cuda:12.4.1-cudnn-devel-ubuntu22.04
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
      python3 python3-pip python3-venv git curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*

RUN python3 -m venv /opt/da3-venv
ENV PATH="/opt/da3-venv/bin:$PATH"
RUN pip install --no-cache-dir --upgrade pip wheel setuptools

# Trio wg README DA3 (domyślne wheel'e torch na Linux zawierają CUDA).
RUN pip install --no-cache-dir xformers "torch>=2" torchvision

# Pakiet DA3 z repo (editable wg README), pinowany commit. NORMALNA izolacja builda
# (DA3 to pakiet hatchling — pip sam dociąga hatchling/hatch-vcs; `--no-build-isolation`
# wysypywał build bo venv nie miał hatchling). To ciągnie ciężkie deps DA3
# (open3d, pycolmap, opencv, e3nn...).
RUN git clone https://github.com/ByteDance-Seed/depth-anything-3 /opt/depth-anything-3 \
    && git -C /opt/depth-anything-3 checkout 41736238f5bced4debf3f2a12375d2466874866d \
    && pip install --no-cache-dir -e /opt/depth-anything-3

# hf_transfer jako osobny pakiet (nowy huggingface_hub nie ma extra [hf_transfer],
# a env HF_HUB_ENABLE_HF_TRANSFER=1 bez niego wysypuje pobieranie modelu).
RUN pip install --no-cache-dir \
      fastapi==0.115.6 uvicorn==0.34.0 python-multipart==0.0.20 \
      "numpy<2" "pillow>=12" huggingface_hub hf_transfer
ENV HF_HUB_ENABLE_HF_TRANSFER=1

COPY tentaflow-containers/vision/python/depth-da3/server.py /app/server.py
COPY tentaflow-containers/vision/docker/depth-da3/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

HEALTHCHECK --interval=30s --timeout=5s --start-period=600s --retries=3 \
    CMD curl -fsS "http://127.0.0.1:${PORT:-8097}/health" || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
