# tentaflow-containers

Definicje silnikow AI integrowanych z TentaFlow — kontenery Docker, natywne
binarki i bundle Pythonowe, zorganizowane w 12 kategorii funkcjonalnych.

## Struktura katalogu

```
tentaflow-containers/
├── README.md                  # ten plik
├── build-containers.sh        # build dockerow (skanuje */docker/*/build.sh)
├── build-natives.sh           # build natywnych binarek (linux/macos/win matrix)
├── .gitignore                 # ignoruje output/ i */native/*/.build-*/
├── output/                    # wynikowe artefakty natywnych buildow (.tar.gz)
├── sidecar/                   # wspolny sidecar QUIC (Rust crate, embedowany w kazdy docker)
│
├── llm/                       # Large Language Models
├── stt/                       # Speech-to-Text
├── tts/                       # Text-to-Speech
├── embeddings/                # Wektoryzacja tekstu
├── reranker/                  # Rerankowanie wynikow
├── vision/                    # Analiza obrazow (OCR, detection, captioning)
├── image-gen/                 # Generowanie obrazow
├── video-gen/                 # Generowanie wideo
├── music-gen/                 # Generowanie muzyki
├── model-3d-gen/              # Generowanie modeli 3D
├── agents/                    # Autonomiczne agenty (boty meeting, browser)
└── tools/                     # Function calling, MCP servers
```

Kazda kategoria ma identyczna strukture wewnetrzna:

```
<kategoria>/
├── README.md            # opis kategorii i lista planowanych silnikow
├── _services/           # manifesty TOML silnikow (deklaratywny opis)
├── docker/<engine>/     # kontenery Docker (Dockerfile + entrypoint + config)
├── native/<engine>/     # natywne binarki (build.sh klonujacy upstream)
└── python/<engine>/     # bundle Python (bundle.toml + opcjonalny server.py)
```

Podkatalogi `docker/`, `native/`, `python/` istnieja tylko jesli kategoria
ma silniki danego typu. Pusty `_services/` z `.gitkeep` jest w kazdej kategorii.

## Architektura kontenera

Kazdy kontener Docker sklada sie z:

- **Silnik AI** (vLLM, whisper.cpp, Sherpa, XTTS itp.) — uruchamiany w kontenerze,
  nasluchuje na `localhost` (wewnatrz kontenera) na wlasnym API HTTP.
- **Sidecar QUIC** (`tentaflow-sidecar`) — generyczna binarka Rust, nasluchuje
  QUIC na porcie 5000, tlumaczy QUIC/rkyv ↔ lokalne HTTP silnika. Rola wybierana
  przez `/data/config.toml`. Dzieki temu kazdy kontener uzywa tej samej binarki
  a roznia sie tylko silnikiem i konfiguracja.
- **Dockerfile** multistage: builder + runtime.
- **entrypoint.sh** — startuje silnik w tle + sidecar na pierwszym planie.
- **config.default.toml** — rola + aliasy modelow + upstream URL.

Sidecar zyje w `tentaflow-containers/sidecar/` jako wspolny crate Rust —
kazdy Dockerfile go kopiuje przez `COPY tentaflow-containers/sidecar /build/...`
i buduje raz per obraz.

## Jak budowac

### Kontenery Docker

```bash
# wszystkie kontenery
./build-containers.sh

# konkretny kontener (rozwiazuje kategorie po nazwie engine)
./build-containers.sh teams-bot

# pelna sciezka
./build-containers.sh llm/vllm

# cala kategoria
./build-containers.sh --category llm

# build + push do registry
./build-containers.sh --push

# pelny rebuild bez cache
./build-containers.sh --full

# lista dostepnych
./build-containers.sh --list
```

Zmienne srodowiskowe:
- `REGISTRY` — registry docelowe (domyslnie `ghcr.io/slyb00ts`)
- `TAG` — tag obrazu (domyslnie `latest`)

### Natywne binarki

```bash
# wszystkie silniki dla hosta + autodetekcja backendu
./build-natives.sh

# konkretne parametry
./build-natives.sh linux x86_64 cuda
./build-natives.sh macos aarch64 metal
```

Wynikowe `.tar.gz` ladują w `output/` (top-level). Patrz `output/README.md`
po szczegoly nazewnictwa i platform docelowych.

## Manifest serwisow (TOML)

Manifesty `_services/<engine-id>.toml` opisuja silnik deklaratywnie:
identyfikator, kategoria, warianty deployment (`docker`/`native`/`python-bundle`/`embedded`/`external`),
wymagania GPU, mapa portow, aliasy modeli. `tentaflow-core/build.rs` odczytuje
wszystkie manifesty przy `cargo build`, waliduje 9 regul semantycznych i generuje:

- Rust const w `$OUT_DIR/services_generated.rs` — uzywany przez
  `tentaflow-core/src/services/manifest/registry.rs`
- JS module `tentaflow-core/wwwroot/js/generated/services-manifest.js` —
  konsumowany przez `wwwroot/js/modules/catalog/ManifestStore.js` w GUI

Pelna specyfikacja: [`_schema/SCHEMA.md`](./_schema/SCHEMA.md) (sekcje, pola,
podsekcje, enum-y, reguly walidacji, przyklady). JSON Schema do walidacji w
edytorze: [`_schema/schema.json`](./_schema/schema.json).

### Build vs Download

Kazdy `[[variant]]` typu `docker` ma dwie opcje instalacji:

- **Build** — lokalny `docker build` z `[variant.build].context_path`. Zawsze
  dostepne (Free).
- **Download** — pull prebuilt image z `[variant.download].image`. Wymaga
  TentaFlow Pro (sprawdzane przez `tentaflow-core/src/license/checker.rs`).
  W v1 wszystkie `download.enabled = false` — infrastruktura przygotowana
  pod Pro, zadne obrazy nie sa publikowane.

### Jak dodac nowy silnik

Procedura krok po kroku w `_schema/SCHEMA.md` (sekcja "Jak dodac nowy silnik").
W skrocie: utworz katalog buildu (`docker/<id>/` lub `native/<id>/` lub
`python/<id>/`), napisz `_services/<id>.toml` zgodny ze schema, uruchom
`cargo build` w `tentaflow-core/` — walidator zaakceptuje albo zwroci blad.

## Deploy

Sidecar + caly kontekst buildu kazdego kontenera jest **embedowany w binarce
`tentaflow`** przez `include_bytes!` w `tentaflow-core/build.rs`. Uzytkownik
uruchamia tylko `tentaflow`, GUI wola:

```
POST /api/deploy/<container_name>  →  extract embedded context → docker build → docker run
```

Nic nie trzeba klonowac ani budowac recznie.

## Lista kontenerow (po reorganizacji)

| Kategoria | Engine | Lokalizacja |
|-----------|--------|-------------|
| llm | llama-cpp | `llm/docker/llama-cpp/` + `llm/native/llama-cpp/` |
| llm | vllm | `llm/docker/vllm/` + `llm/python/vllm/` |
| llm | sglang | `llm/docker/sglang/` + `llm/python/sglang/` |
| llm | ollama | `llm/docker/ollama/` |
| stt | whisper | `stt/docker/whisper/` + `stt/native/whisper-cpp/` |
| stt | parakeet | `stt/docker/parakeet/` + `stt/python/parakeet/` |
| stt | qwen-asr | `stt/docker/qwen-asr/` + `stt/python/qwen-asr/` |
| tts | sherpa-onnx | `tts/docker/sherpa-onnx/` + `tts/native/sherpa-onnx/` |
| tts | xtts | `tts/docker/xtts/` + `tts/python/xtts/` |
| tts | voxcpm | `tts/docker/voxcpm/` + `tts/python/voxcpm/` |
| embeddings | hf-tei | `embeddings/docker/hf-tei/` + `embeddings/native/text-embeddings/` |
| reranker | bge-reranker | `reranker/docker/bge-reranker/` |
| image-gen | comfyui | `image-gen/docker/comfyui/` + `image-gen/python/comfyui/` |
| image-gen | stable-diffusion-cpp | `image-gen/native/stable-diffusion-cpp/` |
| agents | teams-bot | `agents/docker/teams-bot/` |
