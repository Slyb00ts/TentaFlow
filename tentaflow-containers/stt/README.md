# STT (Speech-to-Text)

Speech-to-Text — rozpoznawanie mowy z audio.

## Struktura

- `_services/*.toml` — manifesty silnikow (deklaratywny opis: warianty, GPU, deployment)
- `docker/<engine>/` — kontenery Docker do uruchomienia
- `native/<engine>/` — natywne binarki (whisper.cpp)
- `python/<engine>/` — bundle Python (venv + serwer FastAPI)

## Obslugiwane silniki (planowane)

- whisper.cpp (native, docker — wszystkie platformy)
- faster-whisper (docker, python — Linux/Windows CUDA)
- NVIDIA Parakeet TDT 0.6B (docker, python — Linux CUDA)
- Qwen3-ASR-1.7B przez vLLM (docker, python — Linux CUDA)

## Jak dodac nowy silnik

1. Utworz `_services/<engine-id>.toml` zgodnie z `tentaflow-containers/_schema/SCHEMA.md`
2. Dla wariantu `docker`: dodaj `docker/<engine-id>/` z Dockerfile + entrypoint.sh + config.default.toml + build.sh
3. Dla wariantu `native`: dodaj `native/<engine-id>/build.sh`
4. Dla wariantu `python-bundle`: dodaj `python/<engine-id>/bundle.toml` + opcjonalnie server.py
5. `cargo build` w tentaflow-core/ zwaliduje TOML i wygeneruje wpisy w GUI
