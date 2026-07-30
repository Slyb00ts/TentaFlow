# ADR number OCR — our own trained reader

A small, dedicated CRNN that reads the digit rows of ADR hazard plates (Kemler
identification number on top, UN number on bottom) on dangerous-goods tank
trucks. It clearly beats the public PP-OCRv5 on real footage (see `RAPORT.md`),
especially on small / rotated frames where PP-OCRv5 reads nothing.

The runtime pipeline splits a detected ADR plate crop into top/bottom rows,
reads each row with this model, and snaps the UN number to the deployment's
`adr-list.json` catalogue (Levenshtein ≤ 1). PP-OCRv5 stays wired as a fallback.

## Model

- CRNN: 5 conv blocks → height collapse → 2× BiLSTM(128) → CTC, alphabet
  `0123456789` + blank. Input grayscale 32×128, one digit row.
- ~1.05 M params, ONNX ≈ 4 MB, opset 17.

## Training

Training lives in ML Studio, not here: open a recognition project →
**Trening → OCR**, pick the OCR attribute (e.g. `kod` on `tablica_adr`) and
start. The service is `ocr-training`
(`tentaflow-containers/training/python/ocr-training/`): it collects rows from the
project's **approved** COCO annotations, mixes them with synthetic rows generated
on the fly (the deployment's `adr-list.json` pairs are the label source) and
exports `adr_ocr.onnx` + `adr_ocr_alphabet.txt` — the two files this runtime
loads.

The CRNN definition (`model.py`) and the synthetic generator (`gen_synth.py`)
live in that service, which is the single implementation. The earlier standalone
`train.py` / `export_onnx.py` were folded into it; the first release of the
reader was trained 100% on synthetic data, which is now just the
`synthetic_per_epoch` > 0, no-real-rows case of the same job.

## Files

- `eval.py` — honest evaluation harness against real crops vs the PP-OCRv5
  baseline (expects a `real_crops/`, `labels.tsv`, `adr-list.json` next to it;
  eval data is provided separately and never committed).
- `RAPORT.md` — measured results and honest limitations.
