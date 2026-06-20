# =============================================================================
# Plik: Dockerfile
# Opis: Kontener ComfyUI (image generation) — direct-http, bez sidecara.
# =============================================================================

# Baza CUDA 13 wymagana przez Blackwell B300 (sm_103); torch cu130 dziala wstecz na starszych GPU.
FROM nvidia/cuda:13.0.3-cudnn-runtime-ubuntu24.04
RUN apt-get update && apt-get install -y --no-install-recommends \
      python3 python3-pip python3-venv python3-dev git curl ca-certificates \
      libgl1 libglib2.0-0 gcc g++ \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /opt
RUN git clone --depth 1 https://github.com/comfyanonymous/ComfyUI.git
WORKDIR /opt/ComfyUI

RUN python3 -m venv /opt/venv
ENV PATH="/opt/venv/bin:$PATH"
# torch z cu130 (jedyne kola dzialajace na sm_103). NIE instalujemy xFormers/SageAttention/
# flash-attn - pinuja stary torch i psuja Blackwell; uruchamiamy z SDPA (--use-pytorch-cross-attention).
RUN pip install --no-cache-dir --upgrade pip wheel \
 && pip install --no-cache-dir torch torchvision torchaudio \
      --index-url https://download.pytorch.org/whl/cu130 \
 && pip install --no-cache-dir -r requirements.txt

COPY tentaflow-containers/image-gen/docker/comfyui/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

HEALTHCHECK --interval=30s --timeout=5s --start-period=120s --retries=3 \
    CMD curl -fsS "http://127.0.0.1:${PORT:-${COMFY_PORT:-8188}}/system_stats" || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
