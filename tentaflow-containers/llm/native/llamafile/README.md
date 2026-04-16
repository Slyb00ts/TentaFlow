# llamafile (placeholder)

Planowane: native build llamafile (cosmocc compile, jedna binarka, dziala bez instalacji).

Status: PLANNED. Manifest w `llm/_services/llamafile.toml` ma wszystkie warianty
oznaczone jako `status = "planned"`. Skrypt buildu zostanie dodany w pozniejszej
iteracji.

## Jak dokonczyc

1. Dodaj `build.sh` z krokami:
   - Pobierz binarki llamafile z https://github.com/Mozilla-Ocho/llamafile/releases
   - Zbuduj cosmocc native dla danej platformy
   - Skopiuj artefakty do `output/llamafile-<platform>/`
2. Zaktualizuj `_services/llamafile.toml` zmieniajac `status = "stable"`.
