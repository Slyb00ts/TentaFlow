# Vision

Analiza obrazow — OCR, detekcja obiektow, captioning, multimodal vision.

## Struktura

- `_services/*.toml` — manifesty silnikow (deklaratywny opis: warianty, GPU, deployment)
- `docker/<engine>/` — kontenery Docker (do dodania)
- `native/<engine>/` — natywne binarki (do dodania)
- `python/<engine>/` — bundle Python (do dodania)

## Status

Kategoria zarezerwowana — silniki beda dodawane sukcesywnie. Patrz
`_schema/SCHEMA.md` zeby dodac pierwszy silnik.

Kandydaci: PaddleOCR, Tesseract, GroundingDINO, YOLOv11, Florence-2,
Qwen2.5-VL, LLaVA-NeXT.
