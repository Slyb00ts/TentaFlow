#!/usr/bin/env python3
# ===== File: llm/patches/dspark/patch_nvfp4_026.py — nvfp4_ds_mla for vLLM 0.26 =====
# Adds the `nvfp4_ds_mla` KV-cache dtype, the one piece of the DSpark runtime
# upstream still does not carry. Everything else the old recipe overlay provided
# — the DSpark speculator, the DeepSeek V4 draft model, the B12X MoE backend —
# is in vLLM 0.26 already, so this replaces ~24k lines of copied files.
#
# The 0.21-era patch also had to force the page envelope back to 584 bytes.
# Upstream now computes exactly that for deepseek_v4 (448B NoPE + 128B RoPE +
# 8B scale), so the geometry stages are gone and only the dtype plumbing remains.
#
# Idempotent; a missing anchor is a hard error naming the file, so a vLLM bump
# fails the build instead of producing a runtime that boots and misreports.
import sys
from pathlib import Path


def _vllm_root() -> Path:
    """Katalog pakietu `vllm`. Rozpoznajemy go po ZAWARTOSCI, nie po nazwie —
    kopia robocza czy checkout moga nazywac sie dowolnie, a nazwa `vllm` bywa
    tez katalogiem nadrzednym repo."""
    if len(sys.argv) > 1:
        p = Path(sys.argv[1]).resolve()
        for cand in (p, p / "vllm"):
            if (cand / "config" / "cache.py").is_file():
                return cand
        raise SystemExit(f"{p} nie wyglada na pakiet vllm (brak config/cache.py)")
    try:
        import vllm  # noqa: PLC0415
    except Exception as exc:  # noqa: BLE001
        raise SystemExit(f"nie znajduje pakietu vllm ({exc}) — podaj sciezke argumentem")
    return Path(vllm.__file__).resolve().parent


root = _vllm_root()
applied, skipped = [], []


def replace(path: str, old: str, new: str, label: str) -> None:
    p = root / path
    text = p.read_text()
    if new in text:
        skipped.append(label)
        return
    if old not in text:
        raise SystemExit(
            f"missing patch anchor in {p} [{label}]\n"
            f"upstream przesunal ten kod — przenies latke i zaktualizuj PINNED_VLLM"
        )
    p.write_text(text.replace(old, new, 1))
    applied.append(label)


# --- 1. dtype registration -------------------------------------------------
replace(
    "config/cache.py",
    '    "fp8_ds_mla",',
    '    "fp8_ds_mla",\n    "nvfp4_ds_mla",',
    "cache.py: dozwolony dtype",
)
replace(
    "utils/torch_utils.py",
    '    "fp8_ds_mla": torch.uint8,',
    '    "fp8_ds_mla": torch.uint8,\n    "nvfp4_ds_mla": torch.uint8,',
    "torch_utils.py: mapowanie na uint8",
)

# --- 2. page geometry ------------------------------------------------------
# Both sites already special-case deepseek_v4 at 584B; nvfp4_ds_mla shares that
# envelope. The probe layout is narrower (416B) but the ALLOCATION must stay at
# the proven 584, which is what the 0.21 patch's stage C existed to enforce.
replace(
    "v1/kv_cache_interface.py",
    '        if self.cache_dtype_str == "fp8_ds_mla":\n'
    '            if self.model_version == "deepseek_v4":',
    '        if self.cache_dtype_str in ("fp8_ds_mla", "nvfp4_ds_mla"):\n'
    '            if self.model_version == "deepseek_v4":',
    "kv_cache_interface: real_page_size_bytes (sparse)",
)
replace(
    "v1/kv_cache_interface.py",
    '        if self.model_version == "deepseek_v4" and self.cache_dtype_str == "fp8_ds_mla":',
    '        if self.model_version == "deepseek_v4" and self.cache_dtype_str in (\n'
    '            "fp8_ds_mla",\n'
    '            "nvfp4_ds_mla",\n'
    '        ):',
    "kv_cache_interface: real_page_size_bytes (MLA)",
)

# --- 3. model-side dtype resolution ----------------------------------------
# The resolver asserts fp8 for the block layout; nvfp4_ds_mla is the same paged
# uint8 storage with a different per-token block format, so it takes the same
# branch instead of being rejected by the assert.
replace(
    "models/deepseek_v4/attention.py",
    '    if use_fp8_ds_mla_layout:\n'
    '        # fp8_ds_mla block format: UE8M0 block-scaled fp8 packed as uint8.\n'
    '        assert kv_cache_dtype.startswith("fp8"), (',
    '    if kv_cache_dtype == "nvfp4_ds_mla":\n'
    '        # Same paged uint8 storage as fp8_ds_mla, narrower per-token block.\n'
    '        return kv_cache_dtype, torch.uint8\n'
    '    if use_fp8_ds_mla_layout:\n'
    '        # fp8_ds_mla block format: UE8M0 block-scaled fp8 packed as uint8.\n'
    '        assert kv_cache_dtype.startswith("fp8"), (',
    "attention.py: rozpoznanie nvfp4_ds_mla",
)

print(f"nvfp4_ds_mla -> {root}")
for a in applied:
    print(f"  * {a}")
for s in skipped:
    print(f"  = {s} (juz nalozone)")
if not applied and not skipped:
    raise SystemExit("nic nie zrobiono — zestaw jest pusty?")
