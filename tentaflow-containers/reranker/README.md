# Reranker

Rerankowanie wynikow wyszukiwania — cross-encodery do RAG.

## Struktura

- `_services/*.toml` — manifesty silnikow (deklaratywny opis: warianty, GPU, deployment)
- `docker/<engine>/` — kontenery Docker do uruchomienia (BGE Reranker)

## Obslugiwane silniki (planowane)

- BGE Reranker v2 m3 (docker, in-process — wszystkie platformy)
- Cohere Rerank (external API, docker proxy)
- Jina Reranker v2 (docker, in-process — Linux CUDA)
- mxbai-rerank-large (docker — Linux CUDA)

## Jak dodac nowy silnik

1. Utworz `_services/<engine-id>.toml` zgodnie z `tentaflow-containers/_schema/SCHEMA.md`
2. Dla wariantu `docker`: dodaj `docker/<engine-id>/` z Dockerfile + entrypoint.sh + config.default.toml + build.sh
3. Dla wariantu `in-process`: zarejestruj rolę sidecara w `tentaflow-containers/sidecar/src/roles/`
4. `cargo build` w tentaflow-core/ zwaliduje TOML i wygeneruje wpisy w GUI
