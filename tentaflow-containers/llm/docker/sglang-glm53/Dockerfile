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

# Upstream drops the quant config for every modelopt_fp4 NextN block, which is
# right for DeepSeek (BF16 MTP) and fatal here: GLM-5.3's MTP block ships
# NVFP4-packed routed experts, so the draft would build BF16 experts and MTP
# could not load at all.
COPY tentaflow-containers/llm/patches/glm53/patch_nextn_nvfp4.py /tmp/patch_nextn_nvfp4.py
RUN python3 /tmp/patch_nextn_nvfp4.py && rm /tmp/patch_nextn_nvfp4.py

# A DFlash2 drafter cannot attach without a DFLASH-named aux-hidden hook AND an
# mHC contraction: this model runs hc_mult parallel residual streams, so a raw
# capture is hc_mult times too wide for the drafter's projection.
COPY tentaflow-containers/llm/patches/glm53/patch_dflash_capture.py /tmp/patch_dflash_capture.py
RUN python3 /tmp/patch_dflash_capture.py && rm /tmp/patch_dflash_capture.py

# On a 2-node TP group the NEXTN draft worker deadlocks in multimem init: the
# symmetric-memory all-gather probes same-node-ness with a COLLECTIVE that only
# ranks with a published parallel config join — the draft context is exactly the
# unpublished case, so rank 0 sits in broadcast_object_list forever while rank
# 1's draft never joins. The probe's only cross-node outcome is disabling
# multimem anyway, so the patch skips the probe and disables it whenever the
# group spans nodes (single-node keeps multimem unchanged).
COPY tentaflow-containers/llm/patches/glm53/patch_crossnode_symm_mem.py /tmp/patch_crossnode_symm_mem.py
RUN python3 /tmp/patch_crossnode_symm_mem.py && rm /tmp/patch_crossnode_symm_mem.py

# Fabric variables (NCCL_IB_HCA / NCCL_SOCKET_IFNAME / NCCL_IB_GID_INDEX) and the
# HF cache are per node and injected by the cluster deploy, never baked.
ENV PYTORCH_CUDA_ALLOC_CONF=expandable_segments:True \
    HF_HUB_DISABLE_XET=1

# Cluster deploy bypasses the entrypoint (`bash -c <serve command>` built by
# services/deploy/distributed.rs), so the image only has to stay alive.
ENTRYPOINT ["/bin/bash", "-c"]
CMD ["sleep infinity"]
