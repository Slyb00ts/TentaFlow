# =============================================================================
# Plik: Dockerfile
# Opis: Kontener LLM z vLLM 0.23.0 (PyPI wheels), direct-http (bez sidecara).
#       vLLM nasluchuje OpenAI API na 0.0.0.0:8000, Core gada HTTP wprost.
# =============================================================================

# --- vLLM z PyPI ---
# Build-args sterowane przez Core (deploy.docker.arch_variants per arch GPU).
# Defaulty = Blackwell/cu130, zeby goly `docker build` dzialal. CUDA 13.0 + cudnn
# DEVEL (nie runtime) — flashinfer JIT-kompiluje kernele przez nvcc przy starcie;
# runtime image nie ma nvcc -> crash. Devel daje nvcc -> CUDA graphs + flashinfer
# (pelna wydajnosc) bez --enforce-eager. torch cu130 ma fat wheels (sm80..sm120),
# wiec ten sam obraz dziala tez na Ampere/Ada/Hopper.
ARG CUDA_IMAGE=nvidia/cuda:13.0.3-cudnn-devel-ubuntu24.04
ARG TORCH_INDEX=https://download.pytorch.org/whl/cu130
ARG TORCH_VERSION=2.11.0
ARG TORCHVISION_VERSION=0.26.0
ARG TORCHAUDIO_VERSION=2.11.0
ARG VLLM_VERSION=0.23.0
FROM ${CUDA_IMAGE}
ARG TORCH_INDEX
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

# vLLM + torch z indeksu CUDA wybranego przez build-arg (PyPI ma vllm wheel, ale
# torch musimy wziac z pytorch.org bo PyPI ma wariant CPU).
RUN pip install --no-cache-dir \
      --extra-index-url ${TORCH_INDEX} \
      torch==${TORCH_VERSION} torchvision==${TORCHVISION_VERSION} torchaudio==${TORCHAUDIO_VERSION} \
    && pip install --no-cache-dir vllm==${VLLM_VERSION}

# llm-compressor — self-quantyzacja zrodel HF do NVFP4 (presety Bielik). hf CLI
# do pobierania zrodel safetensors. hf_transfer = szybsze rownolegle pobieranie.
RUN pip install --no-cache-dir llmcompressor "huggingface_hub[cli,hf_transfer]"
ENV HF_HUB_ENABLE_HF_TRANSFER=1 \
    QUANT_SCRIPT=/app/quantize_nvfp4.py

COPY tentaflow-containers/llm/docker/vllm/entrypoint.sh /app/entrypoint.sh
COPY tentaflow-containers/llm/docker/_shared/quantize_nvfp4.py /app/quantize_nvfp4.py
COPY tentaflow-containers/llm/docker/_shared/prepare-vllm-model.sh /app/prepare-vllm-model.sh
RUN chmod +x /app/entrypoint.sh /app/prepare-vllm-model.sh

HEALTHCHECK --interval=30s --timeout=5s --start-period=300s --retries=3 \
    CMD curl -fsS "http://127.0.0.1:${VLLM_PORT:-8000}/health" || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
