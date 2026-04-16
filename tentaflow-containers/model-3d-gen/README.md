# 3D Model Generation

Generowanie modeli 3D — text-to-3D, image-to-3D, mesh synthesis.

## Struktura

- `_services/*.toml` — manifesty silnikow (deklaratywny opis: warianty, GPU, deployment)
- `docker/<engine>/` — kontenery Docker (do dodania)
- `python/<engine>/` — bundle Python (do dodania)

## Status

Kategoria zarezerwowana — silniki beda dodawane sukcesywnie. Patrz
`_schema/SCHEMA.md` zeby dodac pierwszy silnik.

Kandydaci: TripoSR, Hunyuan3D-2, InstantMesh, Stable Fast 3D,
Wonder3D, Zero123++.
