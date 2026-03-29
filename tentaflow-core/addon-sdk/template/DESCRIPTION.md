# Template Addon

Przykladowy addon demonstracyjny dla TentaFlow. Pokazuje jak tworzyc addony
korzystajace z SDK, lifecycle hooks, tool calling i flow builder integration.

## Funkcje

- Przykladowe narzedzie "hello" dla LLM tool calling
- Demonstracja lifecycle hooks (on_install, on_start, on_stop)
- Obsluga eventow z event bus
- Panel UI z deklaratywnym renderingiem
- Integracja z flow builder (bloczek "Powitanie")

## Jak uzywac

1. Sklonuj ten szablon
2. Zmien `addon_id` w `manifest.toml`
3. Dodaj wlasne narzedzia w `src/lib.rs`
4. Skompiluj: `cargo build --release --target wasm32-wasi`
5. Zainstaluj przez dashboard lub API

## Kompilacja

```bash
# Wymagany target WASM
rustup target add wasm32-wasi

# Budowanie
cargo build --release --target wasm32-wasi

# Wynikowy plik WASM
ls target/wasm32-wasi/release/tentaflow_addon_template.wasm
```

## Struktura

```
template/
├── Cargo.toml          # Konfiguracja projektu Rust
├── manifest.toml       # Manifest addonu (uprawnienia, konfiguracja)
├── SKILL.md            # Prompt dla LLM (kiedy uzywac narzedzi)
├── DESCRIPTION.md      # Opis addonu (widoczny w marketplace)
├── blocks.json         # Bloczki flow builder
└── src/
    └── lib.rs          # Glowny kod addonu
```
