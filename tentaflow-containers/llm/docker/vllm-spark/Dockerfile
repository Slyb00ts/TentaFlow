# =============================================================================
# Plik: Dockerfile (vllm-spark)
# Opis: vLLM 0.26.0 budowane ze zrodel pod NVIDIA DGX Spark (GB10 Grace
#       Blackwell, aarch64, sm_121a). PyPI wheels vllm/torch celuja w sm_120
#       i crashuja na sm_121 — tu odpalamy lokalny build z
#       TORCH_CUDA_ARCH_LIST=12.1a w devel-image (nvcc + toolchain).
#       Build CUDA kerneli trwa 20-30 min — to jednorazowy koszt obrazu.
# =============================================================================

# --- vLLM 0.26.0 build from source (direct-http, no sidecar) ---
# Devel variant — potrzebujemy nvcc + cuda headers do kompilacji kerneli.
# Runtime variant nie ma toolchainu, vanilla `pip install -e .` by sie wywalil
# na "nvcc not found".
FROM nvidia/cuda:13.0.3-cudnn-devel-ubuntu24.04
# rdma-core/libibverbs: bez nich NCCL nie otworzy libibverbs.so i przy multi-node
# tensor-parallel (Ray) spadnie z RoCE/ConnectX na wolny TCP. Userspace IB verbs
# = high-speed interconnect miedzy nodami clustra (D3 distributed deploy).
RUN apt-get update && apt-get install -y --no-install-recommends \
      python3 python3-pip python3-venv python3-dev \
      git curl ca-certificates ninja-build cmake \
      gcc g++ \
      libibverbs1 librdmacm1 ibverbs-providers rdma-core \
    && rm -rf /var/lib/apt/lists/*

RUN python3 -m venv /opt/vllm-venv
ENV PATH="/opt/vllm-venv/bin:$PATH"
RUN pip install --no-cache-dir --upgrade pip wheel setuptools

# PyTorch 2.11 + cu130 z aarch64 nightly wheels (sm_121 baseline). Stable
# PyPI torch-2.11.0 nie ma sm_121 bytecode dla Grace Blackwell.
RUN pip install --no-cache-dir \
      --extra-index-url https://download.pytorch.org/whl/nightly/cu130 \
      torch==2.11.0 torchvision==0.26.0 torchaudio==2.11.0 \
      ninja cmake

# Build env wymuszony PRZED `pip install -e .` zeby nvcc emitowal sm_121a
# i resolver nie probowal sciagac prebuilt wheela z PyPI. Dwie z tych
# zmiennych zostaje tez w runtime — entrypoint.sh przepuszcza je przez
# `${VAR:-default}`.
ENV TORCH_CUDA_ARCH_LIST="12.1a" \
    VLLM_TARGET_DEVICE="cuda" \
    VLLM_USE_PRECOMPILED="0" \
    VLLM_USE_FLASHINFER_MXFP4_MOE="1" \
    VLLM_SKIP_P2P_CHECK="1" \
    TRITON_PTXAS_PATH="/usr/local/cuda/bin/ptxas" \
    MAX_JOBS="8"

# Klon vllm @ v0.26.0 i build editable. Trzymamy `--depth 1` zeby skrocic
# clone — caly history nie jest potrzebny do builda.
WORKDIR /src
RUN git clone --depth 1 --branch v0.26.0 https://github.com/vllm-project/vllm.git \
    && cd vllm \
    && pip install --no-cache-dir -e . \
    && pip cache purge

# Ray w TYM SAMYM venv co vllm — wymagany do multi-node tensor-parallel
# (head+worker, --distributed-executor-backend ray). 2.55.1 ma kola aarch64 i
# jest kompatybilny z vllm 0.23 / torch 2.11 (D3 distributed deploy).
RUN pip install --no-cache-dir ray==2.55.1

# llm-compressor — self-quantyzacja zrodel HF do NVFP4 (presety Bielik). hf CLI
# do pobierania zrodel. Na Sparku (sm_121a) NVFP4 ma akceleracje sprzetowa.
RUN pip install --no-cache-dir llmcompressor "huggingface_hub[cli,hf_transfer]"
ENV HF_HUB_ENABLE_HF_TRANSFER=1 \
    QUANT_SCRIPT=/app/quantize_nvfp4.py

COPY tentaflow-containers/llm/docker/vllm-spark/entrypoint.sh /app/entrypoint.sh
COPY tentaflow-containers/llm/docker/_shared/quantize_nvfp4.py /app/quantize_nvfp4.py
COPY tentaflow-containers/llm/docker/_shared/prepare-vllm-model.sh /app/prepare-vllm-model.sh
RUN chmod +x /app/entrypoint.sh /app/prepare-vllm-model.sh

HEALTHCHECK --interval=30s --timeout=5s --start-period=300s --retries=3 \
    CMD curl -fsS "http://127.0.0.1:${VLLM_PORT:-8000}/health" || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
