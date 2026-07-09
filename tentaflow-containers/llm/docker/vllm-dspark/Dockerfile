# syntax=docker/dockerfile:1
# ===== File: llm/docker/vllm-dspark/Dockerfile — DeepSeek V4 Flash DSpark NVFP4-KV runtime (DGX Spark, sm_121) =====
# Base image is the prebuilt vLLM 0.24 Spark build ("unholy fusion"); on top of it
# we apply the pinned tonyd2wild two-node recipe: the DSpark overlay (spec-decode
# proposer, B12X MoE/MLA kernels, patched scheduler) and the stage A/B/C
# nvfp4_ds_mla KV-cache patches. The result is the exact runtime validated on
# 2x DGX Spark at 50-60 tok/s (code workload) with the 2026-07-03 garble fix.
FROM ghcr.io/bjk110/vllm-spark:unholy-fusion-prod-ready

ARG DSPARK_RECIPE_SHA=748f0346f92f8bb2de6f97bf49bfd3d8cc193c9a
ADD https://codeload.github.com/tonyd2wild/DeepSeek-v4-Flash-DSpark-1M-NVFP4-KV-2x-DGX-Spark/tar.gz/${DSPARK_RECIPE_SHA} /tmp/dspark-recipe.tar.gz

# Overlay: recipe/overlay/vllm mirrors the site-packages/vllm layout 1:1, so a
# recursive copy applies every file the recipe's overlay Dockerfile COPYs
# (incl. v1/core/sched/scheduler.py == the "patch3" concurrency scheduler and the
# version-matched dspark_proposer.py). py_compile catches a broken copy early.
RUN set -e; \
    mkdir -p /tmp/dspark-recipe; \
    tar -xzf /tmp/dspark-recipe.tar.gz -C /tmp/dspark-recipe --strip-components=1; \
    SP=/opt/env/lib/python3.12/site-packages/vllm; \
    OV=/tmp/dspark-recipe/recipe/overlay/vllm; \
    cp -a "$OV/." "$SP/"; \
    cd "$OV" && find . -name '*.py' -printf "$SP/%P\n" | xargs /opt/env/bin/python -m py_compile; \
    rm -rf /tmp/dspark-recipe /tmp/dspark-recipe.tar.gz

# Stage A/B/C nvfp4_ds_mla patches (dtype plumbing -> DeepSeek V4 probe -> padded
# 584-byte page envelope), applied as one idempotent script.
COPY tentaflow-containers/llm/docker/vllm-dspark/patch_nvfp4_stages.py /tmp/patch_nvfp4_stages.py
RUN /opt/env/bin/python /tmp/patch_nvfp4_stages.py && rm /tmp/patch_nvfp4_stages.py

# Runtime environment of the verified 2026-07-04 deployment. NCCL fabric vars
# (NCCL_IB_HCA / NCCL_SOCKET_IFNAME / NCCL_IB_GID_INDEX / VLLM_HOST_IP) are
# node-specific and injected by the cluster deploy, not baked here.
ENV PATH=/opt/env/bin:/opt/env/nvvm/bin:/opt/env/targets/sbsa-linux/nvvm/bin:$PATH \
    CUDA_HOME=/opt/env/targets/sbsa-linux \
    CUDA_PATH=/opt/env/targets/sbsa-linux \
    CUDAToolkit_ROOT=/opt/env/targets/sbsa-linux \
    LD_LIBRARY_PATH=/opt/env/lib:/opt/env/targets/sbsa-linux/lib \
    TORCH_CUDA_ARCH_LIST=12.1a \
    FLASHINFER_CUDA_ARCH_LIST=12.1a \
    FLASHINFER_DISABLE_VERSION_CHECK=1 \
    TILELANG_CLEANUP_TEMP_FILES=1 \
    DG_JIT_USE_NVRTC=0 \
    DG_JIT_NVCC_COMPILER=/opt/env/bin/nvcc \
    PYTORCH_CUDA_ALLOC_CONF=expandable_segments:True \
    HF_HUB_DISABLE_XET=1 \
    VLLM_ALLOW_LONG_MAX_MODEL_LEN=1 \
    VLLM_TRITON_MLA_SPARSE=1 \
    VLLM_SPARSE_INDEXER_MAX_LOGITS_MB=256 \
    VLLM_MEMORY_PROFILER_ESTIMATE_CUDAGRAPHS=0 \
    VLLM_SKIP_INIT_MEMORY_CHECK=1 \
    VLLM_USE_FLASHINFER_SAMPLER=1 \
    VLLM_USE_B12X_MOE=1 \
    VLLM_USE_B12X_WO_PROJECTION=1 \
    VLLM_B12X_W4A16_FORCE_BLOCKS_PER_SM=0 \
    VLLM_B12X_W4A16_FORCE_BLOCKS_MAX_M=16 \
    B12X_W4A16_TC_DECODE=0 \
    VLLM_DSPARK_CONFIDENCE_THRESHOLD=0.0 \
    VLLM_DSPARK_CONFIDENCE_SCHEDULER=off \
    VLLM_DSPARK_LOCAL_ARGMAX=1 \
    VLLM_DSPARK_REPLICATE_MARKOV_W1=1 \
    VLLM_DSPARK_FUSED_MARKOV_ARGMAX=0 \
    VLLM_DSPARK_GPU_REJECTED_CONTEXT_MASK=1 \
    VLLM_DSPARK_REFERENCE_KV_QUANT_DEQUANT=0 \
    VLLM_DSPARK_HARDWARE_SCHEDULER_EARLY_STOP=1 \
    VLLM_DSV4_B12X_COMPRESSED_MLA=0 \
    VLLM_DSV4_DSPARK_DEFER_TARGET_CAPTURE=0 \
    VLLM_DSV4_DSPARK_DEFER_TARGET_CAPTURE_EXACT=0 \
    NCCL_NET=IB \
    NCCL_IB_DISABLE=0 \
    NCCL_CROSS_NIC=1 \
    NCCL_CUMEM_ENABLE=0 \
    NCCL_IGNORE_CPU_AFFINITY=1 \
    NCCL_DEBUG=WARN \
    NCCL_NVLS_ENABLE=0

COPY tentaflow-containers/llm/docker/vllm-dspark/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh
ENTRYPOINT ["/app/entrypoint.sh"]
