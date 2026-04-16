# Music Generation

Generowanie muzyki i audio — MusicGen, AudioLDM, voice cloning.

## Struktura

- `_services/*.toml` — manifesty silnikow (deklaratywny opis: warianty, GPU, deployment)
- `docker/<engine>/` — kontenery Docker (do dodania)
- `python/<engine>/` — bundle Python (do dodania)

## Status

Kategoria zarezerwowana — silniki beda dodawane sukcesywnie. Patrz
`_schema/SCHEMA.md` zeby dodac pierwszy silnik.

Kandydaci: MusicGen, AudioLDM2, Suno bark, RVC voice cloning,
Stable Audio Open.
