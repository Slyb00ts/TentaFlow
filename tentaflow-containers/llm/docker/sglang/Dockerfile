# =============================================================================
# Plik: Dockerfile
# Opis: Kontener LLM z SGLang (z git), direct-http (bez sidecara).
# =============================================================================

# Build-args sterowane przez Core (deploy.docker.default_build_args).
# Defaulty = Blackwell/cu130 (sm_103/B300): prebuilt sglang-kernel z PyPI ma
# kernele sm90/sm100. sglang 0.5.23 pinuje torch==2.14.1 dokladnie; od 0.5.20 nie
# ma kol pod CUDA 12, wiec obraz jest wylacznie cu130.
ARG CUDA_IMAGE=nvidia/cuda:13.0.3-devel-ubuntu24.04
ARG TORCH_INDEX=https://download.pytorch.org/whl/cu130
ARG TORCH_VERSION=2.14.1
ARG TORCHVISION_VERSION=0.29.1
ARG TORCHAUDIO_VERSION=2.11.0
ARG SGLANG_VERSION=0.5.23
FROM ${CUDA_IMAGE}
ARG TORCH_INDEX
ARG TORCH_VERSION
ARG TORCHVISION_VERSION
ARG TORCHAUDIO_VERSION
ARG SGLANG_VERSION
RUN apt-get update && apt-get install -y --no-install-recommends \
      python3 python3-pip python3-venv python3-dev git curl ca-certificates \
      gcc g++ libnuma1 \
    && rm -rf /var/lib/apt/lists/*

RUN python3 -m venv /opt/sglang-venv
ENV PATH="/opt/sglang-venv/bin:$PATH"
RUN pip install --no-cache-dir --upgrade pip wheel

# torch z indeksu CUDA wybranego przez build-arg, potem sglang.
RUN pip install --no-cache-dir \
      --index-url ${TORCH_INDEX} \
      torch==${TORCH_VERSION} torchvision==${TORCHVISION_VERSION} torchaudio==${TORCHAUDIO_VERSION} \
    && pip install --no-cache-dir "sglang[all]==${SGLANG_VERSION}"

# Obejscie buga "ptxas fatal: sm_103a not defined": Triton dolaczony do torch ma stary ptxas.
# Wymuszamy ptxas z toolkitu CUDA 13 obecnego w obrazie (baza devel).
ENV TRITON_PTXAS_PATH=/usr/local/cuda/bin/ptxas

COPY tentaflow-containers/llm/docker/sglang/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

HEALTHCHECK --interval=30s --timeout=5s --start-period=300s --retries=3 \
    CMD curl -fsS "http://127.0.0.1:${SGLANG_PORT:-30000}/health" || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
