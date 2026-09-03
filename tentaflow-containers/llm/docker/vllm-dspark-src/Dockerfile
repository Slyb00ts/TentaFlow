# syntax=docker/dockerfile:1
# ===== File: llm/docker/vllm-dspark-src/Dockerfile — vLLM built from source for DGX Spark (sm_121a) =====
# Replaces the third-party prebuilt image + 24k-line file overlay with an
# upstream source build plus one patch script we own.
#
# Base is vLLM's own cu130 nightly, used ONLY for its aarch64 toolchain: CUDA 13
# and torch 2.13.0+cu130, exactly what vLLM 0.28 pins. Its own vLLM (0.19.2) is
# too old -- it predates the DSpark speculator -- so we build 0.28 over it and
# skip recompiling torch, which is the expensive half.
#
# Why 0.28 and not the recipe fork: upstream absorbed the pieces the recipe used
# to carry. `v1/worker/gpu/spec_decode/dspark/` ships a DSparkSpeculator,
# registry.py registers DSparkDraftModel, and flashinfer_b12x_moe.py is in-tree.
# What upstream still lacks is the nvfp4_ds_mla KV dtype -- that stays ours.
FROM vllm/vllm-openai:cu130-nightly AS build

ARG VLLM_REF=v0.28.0
ARG TORCH_INDEX=https://download.pytorch.org/whl/cu130
ARG TORCH_VERSION=2.13.0
ARG TORCHVISION_VERSION=0.28.0
ARG TORCHAUDIO_VERSION=2.11.0
# GB10 is sm_121a, but the list here must say plain `12.1`: vLLM's CMake appends
# the `a` itself for the architecture-specific kernels (CMakeLists sm120 block
# maps 12.1 -> 12.1a). Passing `12.1a` here makes torch's parser drop the entry
# and silently fall back to 12.0, which then refuses to compile FlashMLA.
ARG TORCH_CUDA_ARCH_LIST=12.1
ENV TORCH_CUDA_ARCH_LIST=${TORCH_CUDA_ARCH_LIST} \
    FLASHINFER_CUDA_ARCH_LIST=12.1a \
    MAX_JOBS=8 \
    CCACHE_DIR=/ccache \
    CUDA_HOME=/usr/local/cuda \
    VLLM_TARGET_DEVICE=cuda

RUN python3 -m pip install --no-cache-dir \
    --extra-index-url ${TORCH_INDEX} \
    torch==${TORCH_VERSION} torchvision==${TORCHVISION_VERSION} torchaudio==${TORCHAUDIO_VERSION}

# The base is a RUNTIME image: CUDA headers and the unversioned .so symlinks the
# compiler needs are absent, so a source build dies twice -- first on
# CUDA_nvrtc_LIBRARY-NOTFOUND at generate time, then on a missing cublas_v2.h
# after two minutes of compiling. The image already carries NVIDIA's sbsa apt
# repo, so the dev packages come from the same CUDA 13.0 release as the runtime
# ones rather than being stitched in from the pip nvidia-* wheels.
RUN --mount=type=cache,target=/var/cache/apt \
    apt-get update && apt-get install -y --no-install-recommends \
      git ccache \
      cuda-nvrtc-dev-13-0 \
      libcublas-dev-13-0 \
      libcusparse-dev-13-0 \
      libcusolver-dev-13-0 \
      libcurand-dev-13-0 \
      libcufft-dev-13-0 \
    && rm -rf /var/lib/apt/lists/*

# Shallow clone of the pinned tag: we never need the history, and the tree alone
# is ~150 MB against several GB for a full clone.
RUN git clone --depth 1 --branch ${VLLM_REF} https://github.com/vllm-project/vllm.git /src/vllm

WORKDIR /src/vllm
# torch is pinned above before the vLLM source build; reinstalling it from the
# requirements would pull a non-Spark wheel over a working one.
RUN --mount=type=cache,target=/ccache \
    --mount=type=cache,target=/root/.cache/pip \
    sed -i '/^torch==/d;/^torchvision==/d;/^torchaudio==/d' requirements/cuda.txt && \
    sed -i '/^torch *==/d' requirements/build/cuda.txt && \
    python3 -m pip install --no-build-isolation -r requirements/build/cuda.txt && \
    python3 -m pip install --no-build-isolation .

# The base image carries flashinfer-cubin from ITS vLLM (0.19); installing 0.28
# pulls a newer flashinfer and the two are version-locked against each other --
# the worker dies at init with "flashinfer-cubin (x) does not match flashinfer
# (y)". Resolve the cubin from whatever flashinfer actually landed rather than
# pinning a number that goes stale at the next bump.
#
# Read the version from package METADATA, not by importing flashinfer: the very
# check we are fixing runs at import time, so `import flashinfer` raises the
# mismatch instead of reporting it.
RUN --mount=type=cache,target=/root/.cache/pip \
    FI=$(python3 -c "from importlib.metadata import version; print(version('flashinfer'))") && \
    echo "flashinfer=$FI -- dopasowuje cubin" && \
    python3 -m pip install --no-deps --force-reinstall "flashinfer-cubin==$FI" && \
    python3 -c "import flashinfer; print('flashinfer OK', flashinfer.__version__)"

# Drop the checkout. It is build residue, and worse: /src/vllm/vllm SHADOWS the
# installed package on sys.path, so the runtime imports unpatched sources while
# the patch reports success against dist-packages. The image boots, serves, and
# silently rejects nvfp4_ds_mla.
WORKDIR /
RUN rm -rf /src

# The one thing upstream still does not have: the nvfp4_ds_mla KV cache dtype.
# Five anchored edits -- dtype registration, page geometry, model-side resolution.
# The 0.21-era script is NOT usable here: it also forced the 584B page envelope,
# which upstream now computes itself, and its anchors predate the restructure.
# Anchor-based and idempotent -- a vLLM bump that moves an anchor fails the build
# here with the file name instead of producing a runtime that boots and lies.
COPY tentaflow-containers/llm/patches/dspark/patch_nvfp4_ds_mla.py /tmp/patch_nvfp4.py
RUN python3 /tmp/patch_nvfp4.py && rm /tmp/patch_nvfp4.py

# Fabric variables (NCCL_IB_HCA / NCCL_SOCKET_IFNAME / NCCL_IB_GID_INDEX /
# VLLM_HOST_IP) are per node and injected by the cluster deploy, never baked.
ENV PYTORCH_CUDA_ALLOC_CONF=expandable_segments:True \
    VLLM_ALLOW_LONG_MAX_MODEL_LEN=1 \
    VLLM_SKIP_INIT_MEMORY_CHECK=1 \
    HF_HUB_DISABLE_XET=1

ENTRYPOINT ["/bin/bash", "-c"]
CMD ["sleep infinity"]
