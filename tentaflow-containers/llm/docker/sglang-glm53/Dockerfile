# syntax=docker/dockerfile:1
# ===== File: llm/docker/sglang-glm53/Dockerfile — GLM-5.3-Flash NVFP4 on 2x DGX Spark =====
# Per-model image, NOT a general sglang build. `glm5_next` (released 2026-08-26)
# is in neither sglang nor vLLM upstream — sglang#36507 and vllm#53906 are both
# open — so the runtime has to come from the vendor's per-model tag.
#
# Why sglang and not vLLM, on THIS hardware: GLM-5.3-Flash is NoPE
# (`qk_rope_head_dim = 0`), and on sm_121 vLLM's only sparse-MLA backend is
# FLASHINFER_MLA_SPARSE_SM120, whose impl hard-requires the `fp8_ds_mla` page
# layout (512 NoPE + 64 RoPE). The other sparse backends are locked to older
# architectures by `supports_compute_capability`: FLASH_ATTN_MLA_SPARSE major==9,
# FLASHMLA_SPARSE major in [9,10]. So on a Spark vLLM has no kernel that can
# attend this model and returns garbage with a clean finish_reason. sglang's
# TileLang DSA path has a dedicated `tail_dim == 0` kernel.
FROM lmsysorg/sglang:glm-5.3-flash-arm64

# The stock TileLang tile cannot LAUNCH on GB10 (asks 169_984 B of dynamic
# shared memory against a 101_376 B optin cap). Anchored and idempotent: an
# sglang bump that moves the defaults fails the build here instead of shipping
# an image whose attention kernel dies at the first forward pass.
COPY tentaflow-containers/llm/patches/glm53/patch_tilelang_gb10.py /tmp/patch_tilelang_gb10.py
RUN python3 /tmp/patch_tilelang_gb10.py && rm /tmp/patch_tilelang_gb10.py

# Fabric variables (NCCL_IB_HCA / NCCL_SOCKET_IFNAME / NCCL_IB_GID_INDEX) and the
# HF cache are per node and injected by the cluster deploy, never baked.
ENV PYTORCH_CUDA_ALLOC_CONF=expandable_segments:True \
    HF_HUB_DISABLE_XET=1

# Cluster deploy bypasses the entrypoint (`bash -c <serve command>` built by
# services/deploy/distributed.rs), so the image only has to stay alive.
ENTRYPOINT ["/bin/bash", "-c"]
CMD ["sleep infinity"]
