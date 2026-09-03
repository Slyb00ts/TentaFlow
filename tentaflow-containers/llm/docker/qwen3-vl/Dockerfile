# =============================================================================
# Plik: Dockerfile
# Opis: Obraz zamiennika VLM Qwen3-VL = vLLM (PyPI wheels) w trybie
#       generacyjnym chat z obsluga obrazow, direct-http (bez sidecara). Model
#       przyjmuje tekst i obrazy, generuje tekst; vLLM nasluchuje OpenAI API na
#       0.0.0.0:8000 (/v1/chat/completions), Core gada HTTP wprost.
# =============================================================================

# --- vLLM z PyPI ---
# Build-args sterowane przez Core (deploy.docker per arch GPU); defaulty = cu130.
# torch cu130 ma fat wheels (sm80..sm120), wiec ten sam obraz dziala na
# Ampere/Ada/Hopper/Blackwell. Runtime variant (nie devel) bo nie kompilujemy ze
# zrodel — wheels z PyPI maja prebuilt extensions (vllm, xformers, flashinfer).
ARG CUDA_IMAGE=nvidia/cuda:13.0.3-cudnn-runtime-ubuntu24.04
ARG TORCH_INDEX=https://download.pytorch.org/whl/cu130
ARG VLLM_INDEX=https://wheels.vllm.ai/nightly/cu130
ARG TORCH_VERSION=2.13.0
ARG TORCHVISION_VERSION=0.28.0
ARG TORCHAUDIO_VERSION=2.11.0
ARG VLLM_VERSION=0.28.0
FROM ${CUDA_IMAGE}
ARG TORCH_INDEX
ARG VLLM_INDEX
ARG TORCH_VERSION
ARG TORCHVISION_VERSION
ARG TORCHAUDIO_VERSION
ARG VLLM_VERSION
RUN apt-get update && apt-get install -y --no-install-recommends \
      python3 python3-pip python3-venv python3-dev curl ca-certificates \
      gcc g++ \
    && rm -rf /var/lib/apt/lists/*

RUN python3 -m venv /opt/vllm-venv
ENV PATH="/opt/vllm-venv/bin:$PATH"
RUN pip install --no-cache-dir --upgrade pip wheel

# vLLM + torch z indeksow CUDA wybranych przez build-args. torch idzie z
# pytorch.org (PyPI ma wariant CPU); vLLM z wlasnego indeksu (cu130 dla B300).
RUN pip install --no-cache-dir \
      --extra-index-url ${TORCH_INDEX} \
      torch==${TORCH_VERSION} torchvision==${TORCHVISION_VERSION} torchaudio==${TORCHAUDIO_VERSION} \
    && pip install --no-cache-dir \
      --extra-index-url ${VLLM_INDEX} \
      vllm==${VLLM_VERSION}

# hf CLI do pobierania zrodel safetensors z HF.
# hf_transfer = szybsze rownolegle pobieranie.
RUN pip install --no-cache-dir "huggingface_hub[cli,hf_transfer]"
ENV HF_HUB_ENABLE_HF_TRANSFER=1

COPY tentaflow-containers/llm/docker/qwen3-vl/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

HEALTHCHECK --interval=30s --timeout=5s --start-period=300s --retries=3 \
    CMD curl -fsS "http://127.0.0.1:${VLLM_PORT:-8000}/health" || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
