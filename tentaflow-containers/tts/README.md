# TTS (Text-to-Speech)

Text-to-Speech — synteza mowy z tekstu.

## Struktura

- `_services/*.toml` — manifesty silnikow (deklaratywny opis: warianty, GPU, deployment)
- `docker/<engine>/` — kontenery Docker do uruchomienia
- `native/<engine>/` — natywne binarki (sherpa-onnx)
- `python/<engine>/` — bundle Python (venv + serwer FastAPI)

## Obslugiwane silniki (planowane)

- Sherpa ONNX (native, docker — wszystkie platformy, CPU/GPU)
- XTTS v2 (docker, python — voice cloning, Linux/Windows CUDA)
- VoxCPM2 (docker, python — Linux CUDA)
- Piper (native — wszystkie platformy CPU, niskie zasoby)

## Jak dodac nowy silnik

1. Utworz `_services/<engine-id>.toml` zgodnie z `tentaflow-containers/_schema/SCHEMA.md`
2. Dla wariantu `docker`: dodaj `docker/<engine-id>/` z Dockerfile + entrypoint.sh + config.default.toml + build.sh
3. Dla wariantu `native`: dodaj `native/<engine-id>/build.sh`
4. Dla wariantu `python-bundle`: dodaj `python/<engine-id>/bundle.toml` + opcjonalnie server.py
5. `cargo build` w tentaflow-core/ zwaliduje TOML i wygeneruje wpisy w GUI
