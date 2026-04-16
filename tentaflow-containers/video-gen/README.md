# Video Generation

Generowanie wideo — text2video, image2video, motion synthesis.

## Struktura

- `_services/*.toml` — manifesty silnikow (deklaratywny opis: warianty, GPU, deployment)
- `docker/<engine>/` — kontenery Docker (do dodania)
- `python/<engine>/` — bundle Python (do dodania)

## Status

Kategoria zarezerwowana — silniki beda dodawane sukcesywnie. Patrz
`_schema/SCHEMA.md` zeby dodac pierwszy silnik.

Kandydaci: HunyuanVideo, Wan 2.1, CogVideoX, LTX-Video, Mochi-1,
AnimateDiff, Stable Video Diffusion.
