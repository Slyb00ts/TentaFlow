# LLM (Large Language Models)

Silniki generujace tekst — chat, completions, function calling.

## Struktura

- `_services/*.toml` — manifesty silnikow (deklaratywny opis: warianty, GPU, deployment)
- `docker/<engine>/` — kontenery Docker do uruchomienia
- `native/<engine>/` — natywne binarki (llama.cpp)
- `python/<engine>/` — bundle Python (venv + serwer FastAPI)

## Obslugiwane silniki (planowane)

- llama.cpp (native, embedded — wszystkie platformy)
- MLX (embedded — macOS, iOS Apple Silicon)
- vLLM (docker, python — Linux x64 CUDA/ROCm)
- SGLang (docker, python — Linux x64 CUDA)
- Ollama (external, docker — wszystkie platformy)
- TensorRT-LLM (docker — Linux x64 CUDA z FP8)
- llamafile (native — Linux, macOS, Windows)

## Jak dodac nowy silnik

1. Utworz `_services/<engine-id>.toml` zgodnie z `tentaflow-containers/_schema/SCHEMA.md`
2. Dla wariantu `docker`: dodaj `docker/<engine-id>/` z Dockerfile + entrypoint.sh + config.default.toml + build.sh
3. Dla wariantu `native`: dodaj `native/<engine-id>/build.sh`
4. Dla wariantu `python-bundle`: dodaj `python/<engine-id>/bundle.toml` + opcjonalnie server.py
5. `cargo build` w tentaflow-core/ zwaliduje TOML i wygeneruje wpisy w GUI
