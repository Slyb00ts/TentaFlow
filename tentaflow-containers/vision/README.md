# Vision

Analiza obrazow — OCR, detekcja obiektow, captioning, multimodal vision.

## Status: PUSTE

Ta kategoria nie ma jeszcze zaimplementowanych silnikow. Pojawi sie w GUI jako
pusta sekcja z napisem "Wkrotce".

## Struktura

- `_services/*.toml` — manifesty silnikow (deklaratywny opis: warianty, GPU, deployment)
- `docker/<engine>/` — kontenery Docker (do dodania)
- `native/<engine>/` — natywne binarki (do dodania)
- `python/<engine>/` — bundle Python (do dodania)

## Jak dodac pierwszy silnik

1. Utworz `_services/<engine-id>.toml` zgodnie z `tentaflow-containers/_schema/SCHEMA.md`
2. Dla wariantu docker: dodaj `docker/<engine-id>/Dockerfile` + `entrypoint.sh` + `config.default.toml` + `build.sh`
3. Dla wariantu native: dodaj `native/<engine-id>/build.sh`
4. `cargo build` w tentaflow-core/ zwaliduje TOML i wygeneruje wpisy w GUI

## Kandydaci do dodania (przyszle)

- PaddleOCR — szybki OCR multilingual
- Tesseract — klasyczny OCR open-source
- Surya — nowoczesny OCR z layout analysis
- EasyOCR — OCR z prostym API w Pythonie
- Florence-2 — Microsoft vision-language model
- GroundingDINO — open-vocabulary object detection
- YOLOv11 — najnowsza generacja YOLO
- Qwen2.5-VL — multimodal LLM z vision
- LLaVA-NeXT — open-source VLM
