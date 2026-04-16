# Embeddings

Wektoryzacja tekstu — semantyczne wyszukiwanie, RAG.

## Struktura

- `_services/*.toml` — manifesty silnikow (deklaratywny opis: warianty, GPU, deployment)
- `docker/<engine>/` — kontenery Docker do uruchomienia (HF Text Embeddings Inference)
- `native/<engine>/` — natywne binarki (HF TEI Rust + Candle)

## Obslugiwane silniki (planowane)

- HF Text Embeddings Inference (docker, native — wszystkie platformy)
- ONNX Runtime z BGE/E5/Jina (in-process w sidecarze — wszystkie platformy)
- Sentence-Transformers (docker, python — Linux/Windows CUDA)

## Jak dodac nowy silnik

1. Utworz `_services/<engine-id>.toml` zgodnie z `tentaflow-containers/_schema/SCHEMA.md`
2. Dla wariantu `docker`: dodaj `docker/<engine-id>/` z Dockerfile + entrypoint.sh + config.default.toml + build.sh
3. Dla wariantu `native`: dodaj `native/<engine-id>/build.sh`
4. Dla wariantu `in-process`: zarejestruj rolę sidecara w `tentaflow-containers/sidecar/src/roles/`
5. `cargo build` w tentaflow-core/ zwaliduje TOML i wygeneruje wpisy w GUI
