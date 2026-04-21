# Music Generation

Generowanie muzyki i audio — MusicGen, AudioLDM, voice cloning.

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

- MusicGen — Meta foundation music generation
- AudioLDM2 — text-to-audio diffusion
- Bark — Suno text-to-speech/audio
- OpenVoice — voice cloning od MyShell
- Stable Audio Open — Stability AI audio generation
- RVC — Real-time voice conversion
- Suno Bark — multi-purpose audio generation
