# Service Manifest Schema (TentaFlow)

## 1. Wstęp

**Service Manifest** to plik `*.toml` opisujący jeden silnik wnioskowania (engine) udostępniany
przez TentaFlow — np. vLLM, llama.cpp, sherpa-onnx. Manifest jest **single source of truth**:
opisuje metadane silnika i tryby uruchamiania (Docker, native, external), a wszystkie warstwy
(GUI, build pipeline, runtime) konsumują jego treść.

### Lokalizacja

Każdy manifest leży w katalogu `_services/` odpowiedniej kategorii:

```
tentaflow-containers/
├── llm/_services/llama-cpp.toml
├── llm/_services/vllm.toml
├── tts/_services/sherpa-onnx.toml
├── stt/_services/whisper.toml
└── ...
```

### Konsumenci

| Konsument | Co czyta | Co robi |
|-----------|----------|---------|
| `tentaflow-core/build.rs` | wszystkie `*.toml` z `_services/` | waliduje semantycznie, generuje `services_generated.rs` (Rust const z embedded JSON) oraz `www/js/generated/services-manifest.js` (ESM module) |
| `tentaflow-core/src/services/manifest/registry.rs` | embedded JSON | leniwie tworzy `ManifestRegistry` (singleton) |
| Dashboard GUI | `services-manifest.js` | renderuje katalog silników, kafelki w wizardzie deploymentu |
| `build-containers.sh` | manifesty z sekcją `[deploy.docker]` | buduje obrazy lokalnie wg `context_path` |
| `build-natives.sh` | manifesty z `[deploy.native]` runtime=binary | kompiluje natywne binarki wg `binary_path` |

### Auto-discovery kategorii

Dodanie pliku `*.toml` do `<category>/_services/` automatycznie sprawia, że kategoria pojawi się
w GUI. **Pusta kategoria (brak plików `*.toml`) jest ukryta** — np. jeśli nikt nie doda
manifestu do `vision/_services/`, sekcja "Vision" nie pojawi się w wizardzie.

---

## 2. Anatomia pliku TOML

```toml
# Sekcja [engine] — metadata silnika.
[engine]
id = "vllm"                                # REQUIRED: kebab-case [a-z0-9][a-z0-9_-]{0,63}
category = "llm"                           # REQUIRED: enum (patrz tabela)
name = "vLLM"                              # REQUIRED: nazwa wyświetlana w GUI
description_pl = "Serwer LLM..."           # REQUIRED: max 200 znaków
description_en = "LLM server..."           # REQUIRED: max 200 znaków
homepage = "https://github.com/..."        # REQUIRED: URL projektu
license = "Apache-2.0"                     # REQUIRED: SPDX id
icon = "vllm"                              # OPTIONAL: klucz w CatalogIcons.js
default_port = 8000                        # REQUIRED: u16 (1..65535)
api = "openai-compatible"                  # REQUIRED: enum API
version = "0.6.3"                          # REQUIRED: wersja referencyjna
# requires_model = true                    # OPTIONAL: czy wizard ma pokazać krok
                                           # "wybór modelu" (HF/preset). Domyślnie
                                           # dedukowane z category: llm/stt/tts/
                                           # embeddings/vision/*-gen → true;
                                           # agents/tools → false. Jawnie wpisz
                                           # gdy agent potrzebuje modelu lub LLM
                                           # nie pobiera (embedded weights).

# Sekcja [deploy.docker] — opcjonalna; jeśli obecna, w wizardzie pojawi się
# przycisk "Docker".
[deploy.docker]
context_path = "llm/docker/vllm"           # REQUIRED: ścieżka pod tentaflow-containers/
platforms = ["linux", "windows"]           # REQUIRED: array OS-ów
download_image = "ghcr.io/.../vllm:latest" # OPTIONAL (Pro feature)
download_size_mb = 8500                    # OPTIONAL

# Sekcja [deploy.native] — opcjonalna; w wizardzie przycisk "Native".
# `runtime` decyduje o sposobie uruchomienia natywnego silnika.
[deploy.native]
platforms = ["linux", "macos", "windows"]  # REQUIRED
runtime = "embedded"                       # REQUIRED: embedded | binary | python-bundle
feature_flag = "inference-llamacpp"        # gdy runtime = embedded
# binary_path = "tts/native/sherpa-onnx"   # gdy runtime = binary
# bundle_path = "llm/python/vllm"          # gdy runtime = python-bundle

# Sekcja [deploy.external] — opcjonalna; w wizardzie przycisk "External".
[deploy.external]
platforms = ["linux", "macos", "windows"]
detection_binary = "ollama"                # REQUIRED: nazwa binarki w PATH
detection_endpoint = "http://localhost:11434"   # REQUIRED: URL do health check
detection_health_path = "/api/tags"        # OPTIONAL, default "/"

# Sekcja [[model_preset]] — 0 lub więcej rekomendowanych modeli.
[[model_preset]]
id = "qwen3.5-0.8b"                        # REQUIRED
display_name = "Qwen 3.5 0.8B"             # REQUIRED
repo = "Qwen/Qwen3.5-0.8B"                 # REQUIRED: HF repo / GGUF URL / MLX repo
quantization = "Q4_K_M"                    # OPTIONAL
recommended = true                         # OPTIONAL, default false
```

---

## 3. Sekcja `[engine]`

| Pole | Typ | Wymagane | Opis |
|------|-----|----------|------|
| `id` | string (regex `^[a-z0-9][a-z0-9_-]{0,63}$`) | tak | Unikalny identyfikator silnika (globalnie). |
| `category` | enum | tak | Kategoria usługi. Patrz lista poniżej. |
| `name` | string | tak | Nazwa wyświetlana w GUI. |
| `description_pl` | string (max 200) | tak | Opis po polsku. |
| `description_en` | string (max 200) | tak | Opis po angielsku. |
| `homepage` | URL | tak | Strona projektu. |
| `license` | string SPDX | tak | Identyfikator SPDX licencji. |
| `icon` | string | nie | Klucz ikony w `www/js/modules/catalog/icons.js`. Brak = ikona kategorii. |
| `resource_kind` | enum | nie | Wysoki poziom klasy zasobu: `ai` albo `infra`. Brak = `ai`. |
| `requires_model` | bool | nie | Wymusza pokazanie lub ukrycie kroku wyboru modelu w wizardzie. |
| `gpu_supported` | bool | nie | Pozwala ukryć krok wyboru GPU dla silników, które nigdy nie używają GPU. |
| `default_port` | u16 (1–65535) | tak | Domyślny port silnika. |
| `api` | enum | tak | Protokół API. Patrz lista poniżej. |
| `version` | string | tak | Wersja referencyjna silnika. |

### Dozwolone wartości

| Pole | Wartości |
|------|----------|
| `category` | `llm`, `stt`, `tts`, `embeddings`, `reranker`, `vision`, `image-gen`, `video-gen`, `music-gen`, `model-3d-gen`, `agents`, `tools` |
| `resource_kind` | `ai`, `infra` |
| `api` | `openai-compatible`, `ollama-native`, `sherpa-tts`, `sherpa-stt`, `comfyui`, `custom` |

---

## 4. Sekcje deploymentu

Manifest **musi mieć przynajmniej jedną** sekcję deploy: `[deploy.docker]`, `[deploy.native]` lub
`[deploy.external]`. Każda definiuje jedną opcję uruchomienia w wizardzie GUI.

### 4.1. `[deploy.docker]`

| Pole | Typ | Wymagane | Opis |
|------|-----|----------|------|
| `context_path` | string | warunkowo | Ścieżka kontekstu Dockerfile, względem `tentaflow-containers/`. Wymagane gdy deploy uruchamia pojedynczy kontener budowany z `Dockerfile`. |
| `compose_path` | string | warunkowo | Ścieżka do pliku Compose/stack, względem `tentaflow-containers/`. Wymagane gdy jeden kafelek ma uruchamiać wiele kontenerów. |
| `platforms` | array enum OS | tak | Systemy, na których ten obraz może być zbudowany / uruchomiony. |
| `download_image` | string | nie | Referencja OCI prebuilt image (Pro feature). |
| `download_size_mb` | u64 | nie | Rozmiar do pobrania (informacyjnie). |

Dokładnie jedno z pól `context_path` albo `compose_path` musi być ustawione.

### 4.2. `[deploy.native]`

| Pole | Typ | Wymagane | Opis |
|------|-----|----------|------|
| `platforms` | array enum OS | tak | Systemy, na których działa wariant natywny. |
| `runtime` | enum `embedded` / `binary` / `python-bundle` | tak | Sposób uruchomienia. |
| `feature_flag` | string | warunkowo | Wymagane gdy `runtime = embedded`. Cargo feature aktywujący silnik w binarce `tentaflow-core`. |
| `binary_path` | string | warunkowo | Wymagane gdy `runtime = binary`. Katalog pod `tentaflow-containers/` zawierający `build.sh`. |
| `bundle_path` | string | warunkowo | Wymagane gdy `runtime = python-bundle`. Katalog z `bundle.toml` + `server.py`. |

#### Co znaczy `runtime`

- **`embedded`** — silnik wkompilowany bezpośrednio w binarkę `tentaflow` (np. `llama.cpp`,
  `MLX`). Włączany Cargo featurem (`feature_flag`). Niewidoczny dla usera jako osobna binarka.
- **`binary`** — natywna binarka kompilowana skryptem `binary_path/build.sh`, instalowana
  jako sidecar (np. `sherpa-onnx`, `stable-diffusion-cpp`).
- **`python-bundle`** — bundle Pythona (venv + `server.py`) zarządzany przez TentaFlow
  (np. `vllm`, `xtts`).

### 4.3. `[deploy.external]`

| Pole | Typ | Wymagane | Opis |
|------|-----|----------|------|
| `platforms` | array enum OS | tak | Systemy, na których wykrywanie ma sens. |
| `detection_binary` | string | tak | Nazwa binarki w `PATH` (np. `ollama`). |
| `detection_endpoint` | URL | tak | Adres health check (np. `http://localhost:11434`). |
| `detection_health_path` | string | nie | Ścieżka health check, default `/`. |

---

## 5. Sekcja `[[model_preset]]` (opcjonalna)

Lista rekomendowanych modeli dla silnika. Zero lub więcej bloków na plik.

| Pole | Typ | Wymagane | Opis |
|------|-----|----------|------|
| `id` | string | tak | Unikalny w obrębie silnika. |
| `display_name` | string | tak | Nazwa wyświetlana. |
| `repo` | string | tak | Repo HuggingFace, URL GGUF, repo MLX itp. |
| `quantization` | string | nie | Np. `"Q4_K_M"`, `"fp16"`, `"int4"`. |
| `recommended` | bool | nie | Default `false`. Zwykle jeden preset na silnik powinien być `true`. |

---

## 6. Reguły walidacji semantycznej (4 reguły)

Build.rs sprawdza poniższe reguły dla każdego manifestu. Naruszenie = błąd kompilacji
z komunikatem wskazującym plik, sekcję i pole.

| # | Reguła |
|---|--------|
| 1 | `engine.id` musi pasować do regex `^[a-z0-9][a-z0-9_-]{0,63}$` (chroni przed path-traversal/RCE). |
| 2 | Manifest MUSI mieć przynajmniej jedną sekcję deploy (`[deploy.docker]`, `[deploy.native]` lub `[deploy.external]`). |
| 3 | `deploy.native.runtime` musi być spójny z polami: `embedded` ⇒ `feature_flag` (i brak `binary_path`/`bundle_path`); `binary` ⇒ `binary_path` (i brak `feature_flag`/`bundle_path`); `python-bundle` ⇒ `bundle_path` (i brak `feature_flag`/`binary_path`). |
| 4 | Ścieżki muszą istnieć na dysku — `deploy.docker.context_path`, `deploy.native.binary_path`, `deploy.native.bundle_path` (sprawdzane build-time, runtime nie ma dostępu do FS). |

Dodatkowo build.rs egzekwuje **globalną unikalność `engine.id`** w obrębie całego repo.

---

## 7. Jak dodać nowy silnik

1. **Wybierz kategorię** — np. nowy silnik STT trafia do `tentaflow-containers/stt/`.
2. **Utwórz katalogi buildu** w zależności od planowanych trybów:
   - Docker: `<kategoria>/docker/<id>/Dockerfile`
   - Native binary: `<kategoria>/native/<id>/build.sh`
   - Python bundle: `<kategoria>/python/<id>/{bundle.toml, server.py}`
   - Embedded: tylko Cargo feature w `tentaflow-core/Cargo.toml` — żadnego katalogu pod `tentaflow-containers/`.
3. **Stwórz manifest** — `<kategoria>/_services/<id>.toml` zgodny z tym schema.
4. **Uruchom `cargo build`** w `tentaflow-core/`. Build.rs sprawdzi 4 reguły i odmówi
   kompilacji przy błędzie. Po sukcesie w outpucie pojawi się
   `Manifest serwisow: zaladowano N silnikow z N plikow TOML`.
5. **Sprawdź w GUI** — w katalogu usług powinien pojawić się nowy kafelek (kategoria
   automatycznie odsłonięta, jeśli była wcześniej pusta).

### Konwencje nazewnicze

- `engine.id`: kebab-case, krótki, zgodny z nazwą upstream (`vllm`, `llama-cpp`, `sherpa-onnx`).
- `context_path` / `binary_path` / `bundle_path`: zgodne z layoutem `<kategoria>/<tryb>/<id>`.
- `feature_flag`: prefiks `inference-` dla silników wnioskowania (`inference-llamacpp`,
  `inference-mlx`, `inference-whisper`).
