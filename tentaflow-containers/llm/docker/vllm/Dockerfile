# =============================================================================
# Plik: Dockerfile
# Opis: Kontener LLM z vLLM 0.23.0 (PyPI wheels), direct-http (bez sidecara).
#       vLLM nasluchuje OpenAI API na 0.0.0.0:8000, Core gada HTTP wprost.
# =============================================================================

# --- vLLM z PyPI ---
# CUDA 13.0 + cudnn runtime — vLLM 0.23.0 wymaga PyTorch 2.11 + cu130.
# Runtime variant (nie devel) bo nie kompilujemy z source — wheels z PyPI maja
# prebuilt extensions (vllm, xformers, flashinfer).
FROM nvidia/cuda:13.0.3-cudnn-runtime-ubuntu24.04
RUN apt-get update && apt-get install -y --no-install-recommends \
      python3 python3-pip python3-venv python3-dev curl ca-certificates \
      gcc g++ \
    && rm -rf /var/lib/apt/lists/*

RUN python3 -m venv /opt/vllm-venv
ENV PATH="/opt/vllm-venv/bin:$PATH"
RUN pip install --no-cache-dir --upgrade pip wheel

# vLLM 0.23.0 + torch 2.11 z cu130 index (PyPI ma vllm wheel, ale torch
# musimy wziac z pytorch.org bo PyPI ma wariant CPU).
RUN pip install --no-cache-dir \
      --extra-index-url https://download.pytorch.org/whl/cu130 \
      torch==2.11.0 torchvision==0.26.0 torchaudio==2.11.0 \
    && pip install --no-cache-dir vllm==0.23.0

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
