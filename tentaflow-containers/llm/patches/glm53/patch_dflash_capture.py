#!/usr/bin/env python3
# ===== File: llm/patches/glm53/patch_dflash_capture.py — DFLASH aux-hidden hook for GLM-5.3 =====
# DFLASH/DSPARK drafting needs the TARGET to hand back hidden states from the
# layers the drafter was trained against (`target_layer_ids` in the drafter's
# config). sglang looks that capability up by name and refuses to start with:
#
#   ValueError: Model Glm5NextForConditionalGeneration implements neither
#   set_dspark_layers_to_capture nor set_dflash_layers_to_capture
#
# The capability itself is already there: `Glm5NextModel.layers_to_capture`,
# the capture in the decoder loop, and `capture_aux_hidden_states`. Only the
# DFLASH-named entry point is missing, so a DFlash2 drafter cannot attach even
# though everything it needs is implemented.
#
# The body is not invented here: it is byte-for-byte the semantics of
# `DeepseekV2ForCausalLM.set_dflash_layers_to_capture` in the same tree, which
# in turn matches what this model's OWN `set_eagle3_layers_to_capture` does on
# its explicit-layer_ids branch — including the `+1`, which is load-bearing:
# `layers_to_capture` is compared against the loop index BEFORE the layer runs,
# so capturing "the output of layer N" means testing for N+1.
#
# Anchored on the eagle3 setter so the method lands on the right class.
# Idempotent; a missing or ambiguous anchor is a hard error.
import sys
from pathlib import Path

MODEL_REL = "srt/models/glm5_next.py"

ANCHOR = """    def set_eagle3_layers_to_capture(self, layer_ids: Optional[List[int]] = None):
        if not self.pp_group.is_last_rank:
            return
"""

ADDITION = """    def set_dflash_layers_to_capture(self, layer_ids: List[int]):
        if not self.pp_group.is_last_rank:
            return

        if layer_ids is None:
            raise ValueError(
                "DFLASH requires explicit layer_ids for aux hidden capture."
            )

        self.capture_aux_hidden_states = True
        self.model.layers_to_capture = [val + 1 for val in layer_ids]

"""


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

if "set_dflash_layers_to_capture" in text:
    print(f"GLM-5.3 DFLASH capture hook -> {path}\n  = juz nalozone")
    raise SystemExit(0)

occurrences = text.count(ANCHOR)
if occurrences != 1:
    raise SystemExit(
        f"oczekiwano 1 wystapienia settera eagle3 w {path}, znaleziono {occurrences}\n"
        f"upstream przesunal ten kod — przenies latke i zaktualizuj obraz bazowy"
    )

path.write_text(text.replace(ANCHOR, ADDITION + ANCHOR, 1))
print(f"GLM-5.3 DFLASH capture hook -> {path}\n  * set_dflash_layers_to_capture dodane")
