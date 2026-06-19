# =============================================================================
# Plik: Dockerfile
# Opis: Kontener LLM z SGLang (z git), direct-http (bez sidecara).
# =============================================================================

# Baza CUDA 13 devel wymagana przez Blackwell B300 (sm_103); zawiera ptxas z CUDA 13 (sm_103a).
FROM nvidia/cuda:13.0.3-devel-ubuntu24.04
RUN apt-get update && apt-get install -y --no-install-recommends \
      python3 python3-pip python3-venv python3-dev git curl ca-certificates build-essential \
      gcc g++ \
    && rm -rf /var/lib/apt/lists/*

RUN python3 -m venv /opt/sglang-venv
ENV PATH="/opt/sglang-venv/bin:$PATH"
RUN pip install --no-cache-dir --upgrade pip wheel

# SGLang z PyPI wheel (0.5.13, czerwiec 2026 — pierwsza wersja z sgl-kernel pod
# cu130 wspierajaca sm_103 / B300). Torch z cu130 (jedyne kola dla sm_103 / B300).
RUN pip install --no-cache-dir \
      --index-url https://download.pytorch.org/whl/cu130 \
      torch torchvision \
    && pip install --no-cache-dir "sglang[all]==0.5.13"

# Obejscie buga "ptxas fatal: sm_103a not defined": Triton dolaczony do torch ma stary ptxas.
# Wymuszamy ptxas z toolkitu CUDA 13 obecnego w obrazie (baza devel).
ENV TRITON_PTXAS_PATH=/usr/local/cuda/bin/ptxas

COPY tentaflow-containers/llm/docker/sglang/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

HEALTHCHECK --interval=30s --timeout=5s --start-period=300s --retries=3 \
    CMD curl -fsS "http://127.0.0.1:${SGLANG_PORT:-30000}/health" || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
