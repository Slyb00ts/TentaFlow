# =============================================================================
# Plik: Dockerfile
# Opis: Obraz Qwen3 Reranker = vLLM (PyPI wheels) w trybie score/rerank
#       (--task score), direct-http (bez sidecara). vLLM nasluchuje OpenAI API na
#       0.0.0.0:${VLLM_PORT} (/v1/rerank + /v1/score); Core gada HTTP wprost do
#       host-mapped portu.
# =============================================================================

FROM nvidia/cuda:13.0.3-cudnn-runtime-ubuntu24.04
RUN apt-get update && apt-get install -y --no-install-recommends \
      python3 python3-pip python3-venv python3-dev curl ca-certificates \
      gcc g++ \
    && rm -rf /var/lib/apt/lists/*

RUN python3 -m venv /opt/vllm-venv
ENV PATH="/opt/vllm-venv/bin:$PATH"
RUN pip install --no-cache-dir --upgrade pip wheel

RUN pip install --no-cache-dir \
      --extra-index-url https://download.pytorch.org/whl/cu130 \
      torch==2.13.0 torchvision==0.28.0 torchaudio==2.11.0 \
    && pip install --no-cache-dir \
      --extra-index-url https://wheels.vllm.ai/nightly/cu130 \
      vllm==0.27.1

RUN pip install --no-cache-dir "huggingface_hub[cli,hf_transfer]"
ENV HF_HUB_ENABLE_HF_TRANSFER=1

COPY tentaflow-containers/reranker/docker/qwen3-rerank/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

HEALTHCHECK --interval=30s --timeout=5s --start-period=300s --retries=3 \
    CMD curl -fsS "http://127.0.0.1:${VLLM_PORT:-8000}/health" || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
