# ADR number OCR — our own trained reader

A small, dedicated CRNN that reads the digit rows of ADR hazard plates (Kemler
identification number on top, UN number on bottom) on dangerous-goods tank
trucks. Trained **100% on synthetic data** — no manual text labels needed — and
it clearly beats the public PP-OCRv5 on real footage (see `RAPORT.md`),
especially on small / rotated frames where PP-OCRv5 reads nothing.

The runtime pipeline splits a detected ADR plate crop into top/bottom rows,
reads each row with this model, and snaps the UN number to the deployment's
`adr-list.json` catalogue (Levenshtein ≤ 1). PP-OCRv5 stays wired as a fallback.

## Model

- CRNN: 5 conv blocks → height collapse → 2× BiLSTM(128) → CTC, alphabet
  `0123456789` + blank. Input grayscale 32×128, one digit row.
- ~1.05 M params, ONNX ≈ 4 MB, opset 17.

## Reproduce

Uses the training venv (torch + CUDA):
`tentaflow-containers/training/python/classifier-training/.venv/bin/python`.

```bash
PY=tentaflow-containers/training/python/classifier-training/.venv/bin/python
$PY train.py          # trains on synthetic data generated on the fly (gen_synth.py)
$PY export_onnx.py    # crnn_best.pt -> adr_ocr.onnx (+ adr_ocr_alphabet.txt)
```

## Files

- `gen_synth.py` — synthetic ADR digit-row renderer + augmentation.
- `model.py` — CRNN definition.
- `train.py` — training loop (CTC, AMP, OneCycle).
- `export_onnx.py` — Torch → single-file ONNX export.
- `eval.py` — honest evaluation harness against real crops vs the PP-OCRv5
  baseline (expects a `real_crops/`, `labels.tsv`, `adr-list.json` next to it;
  eval data is provided separately and never committed).
- `RAPORT.md` — measured results and honest limitations.
