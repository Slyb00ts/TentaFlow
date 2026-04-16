# Image Generation

Generowanie obrazow — Stable Diffusion, Flux, ComfyUI.

## Struktura

- `_services/*.toml` — manifesty silnikow (deklaratywny opis: warianty, GPU, deployment)
- `docker/<engine>/` — kontenery Docker (ComfyUI)
- `native/<engine>/` — natywne binarki (stable-diffusion.cpp)
- `python/<engine>/` — bundle Python (ComfyUI)

## Obslugiwane silniki (planowane)

- ComfyUI (docker, python — Linux/Windows CUDA, macOS Metal)
- stable-diffusion.cpp (native — wszystkie platformy CUDA/Vulkan/Metal/CPU)
- Diffusers serwer (docker, python — Linux CUDA)
- Flux ONNX (docker, native — Linux/Windows CUDA)

## Jak dodac nowy silnik

1. Utworz `_services/<engine-id>.toml` zgodnie z `tentaflow-containers/_schema/SCHEMA.md`
2. Dla wariantu `docker`: dodaj `docker/<engine-id>/` z Dockerfile + entrypoint.sh + config.default.toml + build.sh
3. Dla wariantu `native`: dodaj `native/<engine-id>/build.sh`
4. Dla wariantu `python-bundle`: dodaj `python/<engine-id>/bundle.toml`
5. `cargo build` w tentaflow-core/ zwaliduje TOML i wygeneruje wpisy w GUI
