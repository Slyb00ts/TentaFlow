# Video Generation

Generowanie wideo — text2video, image2video, motion synthesis.

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

- AnimateDiff — animacje SD z motion modules
- CogVideoX — open-source text-to-video od Tsinghua
- ModelScope-T2V — Alibaba T2V baseline
- HunyuanVideo — Tencent foundation video model
- Wan 2.1 — Alibaba Wan Video model
- LTX-Video — szybkie video gen na consumer GPU
- Mochi-1 — Genmo open-source video gen
- Stable Video Diffusion — Stability AI img2video
