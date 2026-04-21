# Tools

Function calling, MCP servers, integracje z zewnetrznymi API.

## Status: PUSTE

Ta kategoria nie ma jeszcze zaimplementowanych silnikow. Pojawi sie w GUI jako
pusta sekcja z napisem "Wkrotce".

## Struktura

- `_services/*.toml` — manifesty narzedzi (deklaratywny opis: schema funkcji, MCP, deployment)
- `docker/<engine>/` — kontenery Docker (do dodania)

## Jak dodac pierwsze narzedzie

1. Utworz `_services/<engine-id>.toml` zgodnie z `tentaflow-containers/_schema/SCHEMA.md`
2. Dla wariantu docker: dodaj `docker/<engine-id>/Dockerfile` + `entrypoint.sh` + `config.default.toml` + `build.sh`
3. `cargo build` w tentaflow-core/ zwaliduje TOML i wygeneruje wpisy w GUI

## Kandydaci do dodania (przyszle)

- MCP Filesystem Server — bezpieczny dostep do plikow
- MCP Git Server — operacje git przez MCP
- MCP Web Fetch — fetching URL
- Web Search (SearxNG) — meta-wyszukiwarka
- Web Search (Brave) — Brave Search API
- Calculator — obliczenia matematyczne
- Code Interpreter — sandboxed Python execution
- Web Scraper — fetching i parsowanie HTML/JSON
- SQL Query Tool — bezpieczne zapytania do baz
- Calendar API — Google/Outlook calendar
