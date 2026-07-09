# ===== File: llm/docker/vllm-dspark/patch_nvfp4_stages.py — nvfp4_ds_mla stage A/B/C patches =====
# Verbatim replacement payloads from the pinned recipe's
# recipe/nvfp4/Dockerfile.stage-{a,b,c}, applied sequentially in one pass.
# Stage A: plumb the "nvfp4_ds_mla" kv-cache dtype through config/torch utils.
# Stage B: allow it on the DeepSeek V4 MLA attention path (416B probe layout).
# Stage C: pad the page envelope back to DeepSeek V4's proven 584B layout.
# Each replace is idempotent (skips when `new` is already present) and fails
# loudly when an anchor is missing (upstream image drifted -> rebuild the recipe).
from pathlib import Path

root = Path("/opt/env/lib/python3.12/site-packages/vllm")


def replace(path: str, old: str, new: str) -> None:
    p = root / path
    text = p.read_text()
    if new in text:
        return
    if old not in text:
        raise SystemExit(f"missing patch anchor in {p}: {old!r}")
    p.write_text(text.replace(old, new, 1))


# ---------------- Stage A ----------------
replace(
    "config/cache.py",
    '    "fp8_ds_mla",\n    "turboquant_k8v4",',
    '    "fp8_ds_mla",\n    "nvfp4_ds_mla",\n    "turboquant_k8v4",',
)

replace(
    "utils/torch_utils.py",
    '    "fp8_ds_mla": torch.uint8,\n    "turboquant_k8v4": torch.uint8,',
    '    "fp8_ds_mla": torch.uint8,\n    "nvfp4_ds_mla": torch.uint8,\n    "turboquant_k8v4": torch.uint8,',
)

replace(
    "utils/torch_utils.py",
    '        or kv_cache_dtype == "nvfp4"\n',
    '        or kv_cache_dtype == "nvfp4"\n        or kv_cache_dtype == "nvfp4_ds_mla"\n',
)

replace(
    "v1/kv_cache_interface.py",
    '    if kv_cache_dtype == "nvfp4":\n        return KVQuantMode.NVFP4\n',
    '    if kv_cache_dtype == "nvfp4":\n        return KVQuantMode.NVFP4\n    if kv_cache_dtype == "nvfp4_ds_mla":\n        return KVQuantMode.NVFP4\n',
)

replace(
    "v1/kv_cache_interface.py",
    '        if self.cache_dtype_str == "fp8_ds_mla":\n',
    '        if self.cache_dtype_str == "nvfp4_ds_mla":\n            return self.storage_block_size * 416\n        if self.cache_dtype_str == "fp8_ds_mla":\n',
)

# ---------------- Stage B ----------------
replace(
    "models/deepseek_v4/attention.py",
    """        # TODO(yifan): currently hardcoded for FP8 sparse, make it more generic
        head_bytes = (
            self.nope_head_dim  # 448 fp8 NoPE
            + self.rope_head_dim * 2  # 64 bf16 RoPE
            + self.nope_head_dim // 64  # 7B scale factors
            + 1  # 1B pad
        )
""",
    """        # TODO(yifan): currently hardcoded for FP8 sparse, make it more generic
        head_bytes = (
            self.nope_head_dim  # 448 fp8 NoPE
            + self.rope_head_dim * 2  # 64 bf16 RoPE
            + self.nope_head_dim // 64  # 7B scale factors
            + 1  # 1B pad
        )
        if (
            cache_config is not None
            and cache_config.cache_dtype in ("nvfp4", "nvfp4_ds_mla")
        ):
            # Probe layout from the GLM-5.2 NVFP4 sparse-MLA path.
            head_bytes = 416
""",
)

replace(
    "models/deepseek_v4/attention.py",
    """        assert kv_cache_dtype.startswith("fp8"), (
            f"DeepseekV4 only supports fp8 kv-cache format for now, "
            f"got {kv_cache_dtype}"
        )
""",
    """        assert (
            kv_cache_dtype.startswith("fp8")
            or kv_cache_dtype in ("nvfp4", "nvfp4_ds_mla")
        ), (
            f"DeepseekV4 only supports fp8/nvfp4_ds_mla kv-cache format for now, "
            f"got {kv_cache_dtype}"
        )
""",
)

replace(
    "models/deepseek_v4/attention.py",
    """        # FlashMLA Sparse Attention fp8 backend uses "fp8_ds_mla" kv-cache format
        # Automatically convert fp8 kv-cache format to "fp8_ds_mla"
        if (
            issubclass(self.get_attn_backend(), FlashMLASparseBackend)
            and kv_cache_dtype.startswith("fp8")
            and kv_cache_dtype != "fp8_ds_mla"
        ):
""",
    """        if (
            issubclass(self.get_attn_backend(), FlashMLASparseBackend)
            and kv_cache_dtype in ("nvfp4", "nvfp4_ds_mla")
        ):
            assert cache_config is not None
            cache_config.cache_dtype = "nvfp4_ds_mla"
            kv_cache_dtype = "nvfp4_ds_mla"
            logger.info_once("Using probe DeepSeek V4 nvfp4_ds_mla KV cache format.")

        # FlashMLA Sparse Attention fp8 backend uses "fp8_ds_mla" kv-cache format
        # Automatically convert fp8 kv-cache format to "fp8_ds_mla"
        if (
            issubclass(self.get_attn_backend(), FlashMLASparseBackend)
            and kv_cache_dtype.startswith("fp8")
            and kv_cache_dtype != "fp8_ds_mla"
        ):
""",
)

replace(
    "models/deepseek_v4/nvidia/flashmla.py",
    """        if cache_dtype_str == "fp8_ds_mla":
            # DeepseekV4 main MLA: 584B per token (448 NoPE + 128 RoPE + 8 fp8 scale).
            # head_size passed in is the semantic head_dim (512).
            return (num_blocks, block_size, 584)
        else:
            return (num_blocks, block_size, head_size)
""",
    """        if cache_dtype_str == "nvfp4_ds_mla":
            # Probe layout from GLM-5.2 NVFP4 sparse MLA. This is expected to
            # expose whether DeepSeek V4 has compatible store/decode kernels.
            return (num_blocks, block_size, 416)
        if cache_dtype_str == "fp8_ds_mla":
            # DeepseekV4 main MLA: 584B per token (448 NoPE + 128 RoPE + 8 fp8 scale).
            # head_size passed in is the semantic head_dim (512).
            return (num_blocks, block_size, 584)
        else:
            return (num_blocks, block_size, head_size)
""",
)

# ---------------- Stage C ----------------
replace(
    "models/deepseek_v4/attention.py",
    """        if (
            cache_config is not None
            and cache_config.cache_dtype in ("nvfp4", "nvfp4_ds_mla")
        ):
            # Probe layout from the GLM-5.2 NVFP4 sparse-MLA path.
            head_bytes = 416
""",
    """        if (
            cache_config is not None
            and cache_config.cache_dtype in ("nvfp4", "nvfp4_ds_mla")
        ):
            # Stage C: keep DeepSeek V4's proven 584-byte cache envelope so
            # hybrid MLA/SWA grouping can proceed while testing nvfp4_ds_mla.
            head_bytes = 584
""",
)

replace(
    "models/deepseek_v4/nvidia/flashmla.py",
    """        if cache_dtype_str == "nvfp4_ds_mla":
            # Probe layout from GLM-5.2 NVFP4 sparse MLA. This is expected to
            # expose whether DeepSeek V4 has compatible store/decode kernels.
            return (num_blocks, block_size, 416)
""",
    """        if cache_dtype_str == "nvfp4_ds_mla":
            # Stage C: DeepSeek V4 padded NVFP4 probe. Match the fp8_ds_mla
            # envelope first; if this boots, kernel/store correctness is next.
            return (num_blocks, block_size, 584)
""",
)

replace(
    "v1/kv_cache_interface.py",
    """        if self.cache_dtype_str == "nvfp4_ds_mla":
            return self.storage_block_size * 416
        if self.cache_dtype_str == "fp8_ds_mla":
""",
    """        if self.cache_dtype_str == "nvfp4_ds_mla":
            if self.model_version == "deepseek_v4":
                return self.storage_block_size * 584
            return self.storage_block_size * 416
        if self.cache_dtype_str == "fp8_ds_mla":
""",
)

# ---------------- Stage D: restore VLLM_SKIP_INIT_MEMORY_CHECK ----------------
# The recipe's verified profile runs gpu_memory_utilization=0.85 with
# VLLM_SKIP_INIT_MEMORY_CHECK=1, but this vLLM build lost the flag: request_memory
# hard-fails when free < util*total. On GB10 unified memory the OS page cache
# (e.g. after reading the 167GB checkpoint) counts as "used" yet is fully
# reclaimable when cudaMalloc asks, so the startup check is a false negative.
replace(
    "v1/worker/utils.py",
    """    if init_snapshot.free_memory < requested_memory:
        raise ValueError(""",
    """    import os as _os

    if (
        _os.environ.get("VLLM_SKIP_INIT_MEMORY_CHECK") != "1"
        and init_snapshot.free_memory < requested_memory
    ):
        raise ValueError(""",
)

print("nvfp4_ds_mla stage A/B/C/D patches applied")
