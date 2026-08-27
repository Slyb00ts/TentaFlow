#!/usr/bin/env python3
# ===== File: llm/patches/glm53/patch_nextn_nvfp4.py — keep the GLM-5.3 MTP block quantized =====
# `DeepseekModelNextN.__init__` drops the quant config outright for any
# modelopt_fp4 checkpoint:
#
#     if quant_config is not None and quant_config.get_name() == "modelopt_fp4":
#         quant_config = None
#
# That is correct for DeepSeek's own NVFP4 checkpoints, whose NextN block ships
# in BF16. GLM-5.3-Flash-NVFP4 is the opposite case: `Glm5NextForConditional-
# GenerationNextN` inherits this class, and its NextN block (layer 45, next to
# the 45 trunk layers) carries NVFP4-packed routed experts — gate_proj is
# [2048, 2048] uint8, i.e. 4096 logical inputs at two values per byte. With the
# quant config gone the draft builds BF16 experts and the load dies on
# "The size of tensor a (4096) must match the size of tensor b (2048)", so MTP
# cannot start at all.
#
# Nothing is force-quantized here: the checkpoint's `quantization_config.ignore`
# already carries wildcard entries (`*.self_attn.*`, `*.eh_proj`, `*.mlp.gate`,
# `*.mlp.shared_experts.*`) that match layer 45 too, so attention, eh_proj and
# the shared expert stay BF16 exactly as the checkpoint declares. Only the
# routed experts — the tensors that are actually packed — get the NVFP4 path.
#
# Scoped to GLM-5.3 so DeepSeek checkpoints keep the upstream behaviour. Two
# signals, because neither alone is reliable: `architectures` is absent from the
# checkpoint's `text_config` and only set at runtime by `_config_draft_model`,
# while `model_type` ("glm5_next_text") is intrinsic to the file.
# Idempotent; a missing or ambiguous anchor is a hard error.
import sys
from pathlib import Path

MODEL_REL = "srt/models/deepseek_nextn.py"

STOCK = '''        if quant_config is not None and quant_config.get_name() == "modelopt_fp4":
            logger.warning(
                "Overriding DeepseekV3ForCausalLMNextN quant config for modelopt_fp4 Deepseek model."
            )
            quant_config = None
'''

PATCHED = '''        _nextn_arch = str((getattr(config, "architectures", None) or [""])[0])
        _nextn_model_type = str(getattr(config, "model_type", "") or "")
        _nextn_block_is_packed = _nextn_arch.startswith(
            "Glm5Next"
        ) or _nextn_model_type.startswith("glm5_next")
        if (
            quant_config is not None
            and quant_config.get_name() == "modelopt_fp4"
            and not _nextn_block_is_packed
        ):
            logger.warning(
                "Overriding DeepseekV3ForCausalLMNextN quant config for modelopt_fp4 Deepseek model."
            )
            quant_config = None
'''


def _sglang_root() -> Path:
    """Katalog pakietu `sglang`, rozpoznawany po ZAWARTOSCI, nie po nazwie."""
    if len(sys.argv) > 1:
        p = Path(sys.argv[1]).resolve()
        for cand in (p, p / "sglang"):
            if (cand / MODEL_REL).is_file():
                return cand
        raise SystemExit(f"{p} nie wyglada na pakiet sglang (brak {MODEL_REL})")
    try:
        import sglang  # noqa: PLC0415
    except Exception as exc:  # noqa: BLE001
        raise SystemExit(f"nie znajduje pakietu sglang ({exc}) — podaj sciezke argumentem")
    return Path(sglang.__file__).resolve().parent


root = _sglang_root()
path = root / MODEL_REL
text = path.read_text()

if "_nextn_block_is_packed" in text:
    print(f"GLM-5.3 NextN NVFP4 -> {path}\n  = juz nalozone")
    raise SystemExit(0)

occurrences = text.count(STOCK)
if occurrences != 1:
    raise SystemExit(
        f"oczekiwano 1 wystapienia override'u quant configu w {path}, znaleziono {occurrences}\n"
        f"upstream przesunal ten kod — przenies latke i zaktualizuj obraz bazowy"
    )

path.write_text(text.replace(STOCK, PATCHED, 1))
print(f"GLM-5.3 NextN NVFP4 -> {path}\n  * quant config zachowany dla Glm5Next*NextN (MTP w NVFP4)")
