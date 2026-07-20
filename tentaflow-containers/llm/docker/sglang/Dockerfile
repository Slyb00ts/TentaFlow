# =============================================================================
# Plik: Dockerfile
# Opis: Kontener LLM z SGLang (z git), direct-http (bez sidecara).
# =============================================================================

# Build-args sterowane przez Core (deploy.docker.arch_variants per arch GPU).
# Defaulty = Blackwell/cu130 (sm_103/B300): prebuilt sgl-kernel ma kernele sm90/sm100.
# Ampere/Ada (sm80/86/89): prebuilt cu130 sgl-kernel NIE ma tych arch. Wg docs
# sglang (docs.sglang.io/.../install) dla CUDA 12 instaluje sie torch cu129 +
# sgl-kernel z WLASNEGO indeksu sglang (docs.sglang.ai/whl/cu129/), ktory pokrywa
# sm75+ (czyli Ampere). Core nadpisuje wtedy CUDA_IMAGE/TORCH_INDEX na cu12 i
# ustawia SGL_KERNEL_INDEX — bez kompilacji ze zrodel.
ARG CUDA_IMAGE=nvidia/cuda:13.0.3-devel-ubuntu24.04
ARG TORCH_INDEX=https://download.pytorch.org/whl/cu130
ARG SGLANG_VERSION=0.5.15.post1
# Pusty = zostaw prebuilt sgl-kernel z sglang[all] (Hopper/Blackwell, cu130).
# Ustawiony (np. https://docs.sglang.ai/whl/cu129/) = force-reinstall prebuilt
# sgl-kernel z indeksu sglang pod CUDA 12 (Ampere/Ada, sm75+).
ARG SGL_KERNEL_INDEX=
FROM ${CUDA_IMAGE}
ARG TORCH_INDEX
ARG SGLANG_VERSION
ARG SGL_KERNEL_INDEX
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
      torch torchvision \
    && pip install --no-cache-dir "sglang[all]==${SGLANG_VERSION}"

# Ampere/Ada: podmien sgl-kernel na prebuilt z indeksu sglang pod CUDA 12 (pokrywa
# sm75+). Blackwell/Hopper: SGL_KERNEL_INDEX puste → zostaje cu130 sgl-kernel.
RUN if [ -n "${SGL_KERNEL_INDEX}" ]; then \
      echo "[build] reinstalling prebuilt sgl-kernel from ${SGL_KERNEL_INDEX} (Ampere/Ada sm75+)" ; \
      pip install --no-cache-dir --force-reinstall --no-deps sgl-kernel --index-url "${SGL_KERNEL_INDEX}" ; \
    fi

# Obejscie buga "ptxas fatal: sm_103a not defined": Triton dolaczony do torch ma stary ptxas.
# Wymuszamy ptxas z toolkitu CUDA 13 obecnego w obrazie (baza devel).
ENV TRITON_PTXAS_PATH=/usr/local/cuda/bin/ptxas

COPY tentaflow-containers/llm/docker/sglang/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

HEALTHCHECK --interval=30s --timeout=5s --start-period=300s --retries=3 \
    CMD curl -fsS "http://127.0.0.1:${SGLANG_PORT:-30000}/health" || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
