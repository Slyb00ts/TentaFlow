# =============================================================================
# Plik: Dockerfile
# Opis: Obraz NVIDIA Llama-Nemotron Embed = vLLM (PyPI wheels) w trybie
#       embeddings (--task embed), direct-http (bez sidecara). vLLM nasluchuje
#       OpenAI API na 0.0.0.0:${VLLM_PORT} (/v1/embeddings); Core gada HTTP
#       wprost do host-mapped portu.
# =============================================================================

# --- vLLM z PyPI ---
# CUDA 13.0 + cudnn runtime — vLLM 0.26.0 wymaga PyTorch 2.11 + cu130. Runtime
# variant (nie devel) bo nie kompilujemy z source — wheels z PyPI maja prebuilt
# extensions (vllm, xformers, flashinfer).
FROM nvidia/cuda:13.0.3-cudnn-runtime-ubuntu24.04
RUN apt-get update && apt-get install -y --no-install-recommends \
      python3 python3-pip python3-venv python3-dev curl ca-certificates \
      gcc g++ \
    && rm -rf /var/lib/apt/lists/*

RUN python3 -m venv /opt/vllm-venv
ENV PATH="/opt/vllm-venv/bin:$PATH"
RUN pip install --no-cache-dir --upgrade pip wheel

# vLLM 0.26.0 + torch 2.11 z cu130 index. torch idzie z pytorch.org (PyPI ma
# wariant CPU). vLLM wymaga jawnego wheela +cu130 dla B300 (sm_103) — domyslny
# wheel PyPI to cu129, dlatego bierzemy go z indeksu nightly cu130 vLLM.
RUN pip install --no-cache-dir \
      --extra-index-url https://download.pytorch.org/whl/cu130 \
      torch==2.11.0 torchvision==0.26.0 torchaudio==2.11.0 \
    && pip install --no-cache-dir \
      --extra-index-url https://wheels.vllm.ai/nightly/cu130 \
      vllm==0.26.0

# hf CLI do pobierania zrodel safetensors z HF.
# hf_transfer = szybsze rownolegle pobieranie.
RUN pip install --no-cache-dir "huggingface_hub[cli,hf_transfer]"
ENV HF_HUB_ENABLE_HF_TRANSFER=1

COPY tentaflow-containers/embeddings/docker/nemotron-embed/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

HEALTHCHECK --interval=30s --timeout=5s --start-period=300s --retries=3 \
    CMD curl -fsS "http://127.0.0.1:${VLLM_PORT:-8000}/health" || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
