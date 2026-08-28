#!/usr/bin/env python3
# ===== File: llm/patches/glm53/patch_dflash_capture.py — DFLASH aux capture for GLM-5.3 =====
# Lets a DFlash2 drafter attach to GLM-5.3-Flash. Three edits, all modelled on
# `deepseek_v4.py` IN THE SAME TREE — the other mHC model that already feeds a
# speculative drafter — so the semantics are sglang's own, not invented here.
#
# 1. `set_dflash_layers_to_capture`. sglang looks the hook up by name and
#    otherwise refuses to start. `+1` is load-bearing and is NOT a copy of the
#    deepseek_v4 setter (which has no offset): this model tests
#    `layers_to_capture` BEFORE running layer i, so "the output of layer N" is
#    index N+1, whereas deepseek_v4 tests AFTER the layer and needs no shift.
#    Both land on the same tensor. `deepseek_v2.set_dflash_layers_to_capture`
#    carries the same `+1` for the same reason.
#
# 2. A None `residual`. The capture site assumes it is always set; in a hybrid
#    stack a layer that folded the residual in leaves it None, and the capture
#    then dies on `unsupported operand type(s) for +: 'Tensor' and 'NoneType'`.
#    When it is None the hidden state ALREADY IS the full stream — the same case
#    the model handles explicitly at its final norm.
#
# 3. mHC contraction, the reason a drafter could not attach at all. GLM-5.3 runs
#    `hc_mult`(=4) parallel residual streams, so a captured layer is
#    `[tokens, hc_mult*hidden]` (16384) while the drafter's `fc` wants `hidden`
#    (4096) per captured layer — it fails with a feature-dim mismatch of exactly
#    hc_mult. deepseek_v4 contracts its own mHC capture with `completed.mean(dim=1)`
#    over a `[tokens, hc_mult, hidden]` tensor; here the stream axis is flattened
#    (see the `hc_attn_pre` docstring: `[s, hc_mult*hidden]`), and it is
#    stream-major — deepseek_v4 produces the same flat form with `flatten(1)` —
#    so `view(-1, hc_mult, hidden)` is its exact inverse.
#
# Correctness is checkable at runtime and cannot corrupt output: speculative
# decoding verifies exactly, so a wrong contraction costs ACCEPTANCE, never
# tokens. Watch `accept len` / `accept rate` in the serve log — a mis-contracted
# capture collapses acceptance toward zero instead of degrading quietly.
#
# Idempotent per edit; a missing or ambiguous anchor is a hard error.
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

INIT_ANCHOR = """        self.layers_to_capture = []
"""

INIT_ADDITION = """        self.layers_to_capture = []
        # `Glm5NextModel` keeps no `config` reference, so the mHC stream count
        # has to be resolved here, where `config` is still in scope. 1 means
        # "no contraction needed".
        self.hc_capture_mult = int(config.hc_mult) if getattr(config, "mhc", False) else 1
"""

CAPTURE_STOCK = """                if i in self.layers_to_capture:
                    if self.enable_a2a_moe and i > self.first_k_dense_replace:
                        aux_hidden_state = get_parallel().attn_tp_group.all_gather(
                            hidden_states + residual, dim=0
                        )
                        aux_hidden_states.append(aux_hidden_state)
                    else:
                        aux_hidden_states.append(hidden_states + residual)
"""

CAPTURE_FIXED = """                if i in self.layers_to_capture:
                    # A hybrid layer that folded its residual in leaves it None,
                    # and then the hidden state already IS the full stream.
                    captured = (
                        hidden_states if residual is None else hidden_states + residual
                    )
                    # mHC: contract the hc_mult parallel residual streams down to
                    # one, the way deepseek_v4 contracts its own mHC capture. The
                    # stream axis is flattened here and stream-major, so the view
                    # is the exact inverse of that model's `flatten(1)`.
                    if self.hc_capture_mult > 1:
                        captured = captured.view(
                            captured.shape[0], self.hc_capture_mult, -1
                        ).mean(dim=1)
                    if self.enable_a2a_moe and i > self.first_k_dense_replace:
                        aux_hidden_state = get_parallel().attn_tp_group.all_gather(
                            captured, dim=0
                        )
                        aux_hidden_states.append(aux_hidden_state)
                    else:
                        aux_hidden_states.append(captured)
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
notes = []

if "set_dflash_layers_to_capture" in text:
    notes.append("= hook juz obecny")
else:
    hits = text.count(ANCHOR)
    if hits != 1:
        raise SystemExit(
            f"oczekiwano 1 wystapienia settera eagle3 w {path}, znaleziono {hits}\n"
            f"upstream przesunal ten kod — przenies latke i zaktualizuj obraz bazowy"
        )
    text = text.replace(ANCHOR, ADDITION + ANCHOR, 1)
    notes.append("* set_dflash_layers_to_capture dodane")

if "hc_capture_mult" in text:
    notes.append("= mnoznik mHC juz zapamietany")
else:
    hits = text.count(INIT_ANCHOR)
    if hits != 1:
        raise SystemExit(
            f"oczekiwano 1 wystapienia inicjalizacji layers_to_capture w {path}, znaleziono {hits}\n"
            f"upstream przesunal ten kod — przenies latke i zaktualizuj obraz bazowy"
        )
    text = text.replace(INIT_ANCHOR, INIT_ADDITION, 1)
    notes.append("* mnoznik mHC zapamietany przy inicjalizacji")

if CAPTURE_FIXED in text:
    notes.append("= przechwytywanie juz z kontrakcja mHC")
else:
    hits = text.count(CAPTURE_STOCK)
    if hits != 1:
        raise SystemExit(
            f"oczekiwano 1 wystapienia miejsca przechwytywania w {path}, znaleziono {hits}\n"
            f"upstream przesunal ten kod — przenies latke i zaktualizuj obraz bazowy"
        )
    text = text.replace(CAPTURE_STOCK, CAPTURE_FIXED, 1)
    notes.append("* kontrakcja mHC + odpornosc na pusty residual")

path.write_text(text)
print(f"GLM-5.3 DFLASH aux capture -> {path}")
for n in notes:
    print(f"  {n}")
