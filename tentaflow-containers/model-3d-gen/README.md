# 3D Model Generation

Generowanie modeli 3D — text-to-3D, image-to-3D, mesh synthesis.

## Status: PUSTE

Ta kategoria nie ma jeszcze zaimplementowanych silnikow. Pojawi sie w GUI jako
pusta sekcja z napisem "Wkrotce".

## Struktura

- `_services/*.toml` — manifesty silnikow (deklaratywny opis: warianty, GPU, deployment)
- `docker/<engine>/` — kontenery Docker (do dodania)
- `python/<engine>/` — bundle Python (do dodania)

## Jak dodac pierwszy silnik

1. Utworz `_services/<engine-id>.toml` zgodnie z `tentaflow-containers/_schema/SCHEMA.md`
2. Dla wariantu docker: dodaj `docker/<engine-id>/Dockerfile` + `entrypoint.sh` + `config.default.toml` + `build.sh`
3. Dla wariantu native: dodaj `native/<engine-id>/build.sh`
4. `cargo build` w tentaflow-core/ zwaliduje TOML i wygeneruje wpisy w GUI

## Kandydaci do dodania (przyszle)

- Trellis — Microsoft text/image to 3D z latent representation
- Stable3D — Stability AI 3D foundation model
- Wonder3D — image to 3D z multi-view diffusion
- InstantMesh — szybkie image to 3D mesh
- Hunyuan3D-2 — Tencent 3D generation
- TripoSR — Tripo AI single-image to 3D
- Zero123++ — Stability AI multi-view diffusion
- Stable Fast 3D — szybki 3D na consumer GPU
