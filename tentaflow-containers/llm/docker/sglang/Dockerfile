# =============================================================================
# Plik: Dockerfile
# Opis: Kontener LLM z SGLang (z git), direct-http (bez sidecara).
# =============================================================================

# Build-args sterowane przez Core (deploy.docker.arch_variants per arch GPU).
# Defaulty = Blackwell/cu130 (sm_103/B300). Baza CUDA 13 devel zawiera ptxas z
# CUDA 13 (sm_103a). Na Ampere/Ada Core nadpisuje CUDA_IMAGE/TORCH_INDEX na cu124
# i ustawia SGL_KERNEL_BUILD_ARCH, bo prebuilt sgl-kernel (>=0.3.16) ma kernele
# TYLKO dla sm90/sm100 — na sm80/86/89 trzeba przebudowac sgl-kernel ze zrodel.
ARG CUDA_IMAGE=nvidia/cuda:13.0.3-devel-ubuntu24.04
ARG TORCH_INDEX=https://download.pytorch.org/whl/cu130
ARG SGLANG_VERSION=0.5.13
# Pusty = uzyj prebuilt sgl-kernel (Hopper/Blackwell). Ustawiony (np. "8.0;8.6")
# = przebuduj sgl-kernel ze zrodel pod te architektury (Ampere/Ada).
ARG SGL_KERNEL_BUILD_ARCH=
FROM ${CUDA_IMAGE}
ARG TORCH_INDEX
ARG SGLANG_VERSION
ARG SGL_KERNEL_BUILD_ARCH
RUN apt-get update && apt-get install -y --no-install-recommends \
      python3 python3-pip python3-venv python3-dev git curl ca-certificates build-essential \
      gcc g++ libnuma1 cmake ninja-build \
    && rm -rf /var/lib/apt/lists/*

RUN python3 -m venv /opt/sglang-venv
ENV PATH="/opt/sglang-venv/bin:$PATH"
RUN pip install --no-cache-dir --upgrade pip wheel

# torch z indeksu CUDA wybranego przez build-arg, potem sglang.
RUN pip install --no-cache-dir \
      --index-url ${TORCH_INDEX} \
      torch torchvision \
    && pip install --no-cache-dir "sglang[all]==${SGLANG_VERSION}"

# Ampere/Ada: przebuduj DOKLADNIE te wersje sgl-kernel co pinuje sglang, ale ze
# zrodel pod wskazana architekture (prebuilt nie ma sm80/86/89). Blackwell/Hopper
# pomijaja ten krok (SGL_KERNEL_BUILD_ARCH puste) i zostaja na prebuilt.
RUN if [ -n "${SGL_KERNEL_BUILD_ARCH}" ]; then \
      KV=$(pip show sgl-kernel | awk '/^Version:/{print $2}') ; \
      echo "[build] rebuilding sgl-kernel==$KV from source for arch ${SGL_KERNEL_BUILD_ARCH}" ; \
      pip install --no-cache-dir setuptools wheel scikit-build-core ; \
      TORCH_CUDA_ARCH_LIST="${SGL_KERNEL_BUILD_ARCH}" CMAKE_BUILD_PARALLEL_LEVEL="$(nproc)" \
        pip install --no-cache-dir --no-binary sgl-kernel --no-build-isolation \
          --force-reinstall --no-deps "sgl-kernel==$KV" ; \
    fi

# Obejscie buga "ptxas fatal: sm_103a not defined": Triton dolaczony do torch ma stary ptxas.
# Wymuszamy ptxas z toolkitu CUDA 13 obecnego w obrazie (baza devel).
ENV TRITON_PTXAS_PATH=/usr/local/cuda/bin/ptxas

COPY tentaflow-containers/llm/docker/sglang/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

HEALTHCHECK --interval=30s --timeout=5s --start-period=300s --retries=3 \
    CMD curl -fsS "http://127.0.0.1:${SGLANG_PORT:-30000}/health" || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
