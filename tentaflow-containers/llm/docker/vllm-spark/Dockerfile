# =============================================================================
# Plik: Dockerfile (vllm-spark)
# Opis: vLLM 0.20.1 budowane ze zrodel pod NVIDIA DGX Spark (GB10 Grace
#       Blackwell, aarch64, sm_121a). PyPI wheels vllm/torch celuja w sm_120
#       i crashuja na sm_121 — tu odpalamy lokalny build z
#       TORCH_CUDA_ARCH_LIST=12.1a w devel-image (nvcc + toolchain).
#       Build CUDA kerneli trwa 20-30 min — to jednorazowy koszt obrazu.
# =============================================================================

# --- Stage 1: sidecar z Rust ---
FROM rust:slim-bookworm AS sidecar-builder
WORKDIR /build
RUN apt-get update && apt-get install -y --no-install-recommends \
      pkg-config libssl-dev cmake build-essential \
    && rm -rf /var/lib/apt/lists/*
COPY tentaflow-protocol /build/tentaflow-protocol
COPY tentaflow-transport /build/tentaflow-transport
COPY vendor /build/vendor
COPY tentaflow-containers/sidecar /build/tentaflow-containers/sidecar
WORKDIR /build/tentaflow-containers/sidecar
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/tentaflow-containers/sidecar/target \
    cargo build --release --bin tentaflow-sidecar \
    && cp target/release/tentaflow-sidecar /tentaflow-sidecar

# --- Stage 2: vLLM 0.20.1 build from source ---
# Devel variant — potrzebujemy nvcc + cuda headers do kompilacji kerneli.
# Runtime variant nie ma toolchainu, vanilla `pip install -e .` by sie wywalil
# na "nvcc not found".
FROM nvidia/cuda:13.0.3-cudnn-devel-ubuntu24.04
RUN apt-get update && apt-get install -y --no-install-recommends \
      python3 python3-pip python3-venv python3-dev \
      git curl ca-certificates ninja-build cmake \
      gcc g++ \
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
# zmiennych (TORCH_CUDA_ARCH_LIST, VLLM_USE_FLASHINFER_MXFP4_MOE) zostaja
# tez w runtime — entrypoint.sh przepuszcza je przez `${VAR:-default}`.
ENV TORCH_CUDA_ARCH_LIST="12.1a" \
    VLLM_TARGET_DEVICE="cuda" \
    VLLM_USE_PRECOMPILED="0" \
    VLLM_USE_FLASHINFER_MXFP4_MOE="1" \
    VLLM_ATTENTION_BACKEND="FLASHINFER" \
    MAX_JOBS="8"

# Klon vllm @ v0.20.1 i build editable. Trzymamy `--depth 1` zeby skrocic
# clone — caly history nie jest potrzebny do builda.
WORKDIR /src
RUN git clone --depth 1 --branch v0.20.1 https://github.com/vllm-project/vllm.git \
    && cd vllm \
    && pip install --no-cache-dir -e . \
    && pip cache purge

COPY --from=sidecar-builder /tentaflow-sidecar /usr/local/bin/tentaflow-sidecar
COPY tentaflow-containers/llm/docker/vllm-spark/config.default.toml /app/config.default.toml
COPY tentaflow-containers/llm/docker/vllm-spark/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

EXPOSE 5000/udp

HEALTHCHECK --interval=30s --timeout=5s --start-period=300s --retries=3 \
    CMD pgrep tentaflow-sidecar || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
