# Service Manifest Schema (TentaFlow)

## 1. Wstęp

**Service Manifest** to plik `*.toml` opisujący jeden silnik wnioskowania (engine) udostępniany
przez TentaFlow — np. vLLM, llama.cpp, sherpa-onnx. Manifest definiuje:

- metadane silnika (nazwa, licencja, port, API),
- listę wariantów deploymentu (Docker, embedded w binarce, native, zewnętrzny),
- opcjonalne presety modeli (rekomendowane modele dla danego silnika).

### Lokalizacja

Każdy manifest leży w katalogu `_services/` odpowiedniej kategorii:

```
tentaflow-containers/
├── llm/_services/llama-cpp.toml
├── llm/_services/vllm.toml
├── tts/_services/sherpa-onnx.toml
├── stt/_services/<engine>.toml
└── ...
```

### Konsumenci

| Konsument | Co czyta | Co robi |
|-----------|----------|---------|
| `tentaflow-core/build.rs` | wszystkie `*.toml` z `_services/` | embeduje manifesty do binarki, waliduje semantycznie, generuje katalog usług |
| Dashboard GUI | wbudowane manifesty przez REST `/api/v1/catalog` | renderuje listę silników i wariantów |
| `build-containers.sh` | manifesty z `deploy_mode = "docker"` | buduje obrazy lokalnie wg `[variant.build]` |
| `build-natives.sh` | manifesty z `deploy_mode = "native"` | kompiluje natywne binarki wg `[variant.build]` |

---

## 2. Hierarchia pliku TOML

Plik manifestu MA dokładnie jedną sekcję `[engine]` i jeden lub więcej bloków `[[variant]]`.
Może też mieć dowolną liczbę bloków `[[model_preset]]` (opcjonalne).

```toml
[engine]              # dokładnie 1
# ...

[[variant]]           # 1 lub więcej
# ...
[variant.build]       # podsekcja, zależna od deploy_mode
# ...

[[variant]]
# ...
[variant.feature_flag]
# ...

[[model_preset]]      # 0 lub więcej
# ...
```

---

## 3. Sekcja `[engine]`

| Pole | Typ | Wymagane | Opis | Przykład |
|------|-----|----------|------|----------|
| `id` | string (kebab-case `[a-z0-9-]+`) | tak | Unikalny identyfikator silnika w obrębie katalogu (i globalnie). | `"vllm"` |
| `category` | enum | tak | Kategoria usługi. Patrz lista poniżej. | `"llm"` |
| `name` | string | tak | Nazwa wyświetlana w GUI. | `"vLLM"` |
| `description_pl` | string (max 200) | tak | Opis po polsku. | `"Serwer LLM z PagedAttention..."` |
| `description_en` | string (max 200) | tak | Opis po angielsku. | `"Production LLM server..."` |
| `homepage` | URL | tak | Strona projektu. | `"https://github.com/vllm-project/vllm"` |
| `license` | string SPDX | tak | Identyfikator SPDX licencji. | `"Apache-2.0"` |
| `api` | enum | tak | Protokół API. Patrz lista poniżej. | `"openai-compatible"` |
| `default_port` | u16 (1–65535) | tak | Domyślny port silnika. | `8000` |
| `version` | string | tak | Wersja referencyjna silnika. | `"0.6.3"` |
| `tags` | array of string | nie | Etykiety opisowe dla wyszukiwarki. | `["paged-attention", "production"]` |
| `also_serves` | array of category enum | nie | Inne kategorie obsługiwane przez ten sam silnik. | `["embeddings"]` |
| `docs_url` | URL | nie | Adres dokumentacji. | `"https://docs.vllm.ai"` |
| `icon` | string | nie | Klucz ikony w `wwwroot/js/modules/catalog/CatalogIcons.js`. | `"vllm"` |

### Dozwolone wartości `category`

`llm`, `stt`, `tts`, `embeddings`, `reranker`, `vision`, `image-gen`, `video-gen`,
`music-gen`, `model-3d-gen`, `agents`, `tools`.

### Dozwolone wartości `api`

`openai-compatible`, `ollama-native`, `sherpa-tts`, `sherpa-stt`, `comfyui`, `custom`.

---

## 4. Sekcja `[[variant]]`

| Pole | Typ | Wymagane | Opis |
|------|-----|----------|------|
| `id` | string (kebab-case) | tak | Unikalny identyfikator wariantu w obrębie silnika. |
| `deploy_mode` | enum | tak | Tryb deploymentu (patrz niżej). |
| `target_os` | enum lub array | tak | System operacyjny / systemy. |
| `target_arch` | enum lub array | tak | Architektura CPU. |
| `gpu_backend` | enum lub array | tak | Akceleracja GPU. |
| `status` | enum | tak | Status dojrzałości. |
| `vram_gb_min` | u16 | warunkowo | Wymagane gdy `gpu_backend != "cpu"`. |
| `ram_gb_min` | u16 | nie | Minimalna pamięć RAM. |
| `disk_gb_min` | u16 | nie | Minimalne wolne miejsce. |
| `notes_pl` | string | nie | Dodatkowa notka dla użytkownika (po polsku). |
| `notes_en` | string | nie | Dodatkowa notka dla użytkownika (po angielsku). |

### Dozwolone wartości

| Pole | Wartości |
|------|----------|
| `deploy_mode` | `native`, `docker`, `python-bundle`, `embedded`, `external` |
| `target_os` | `linux`, `macos`, `windows`, `ios`, `android` |
| `target_arch` | `x86_64`, `aarch64`, `any` |
| `gpu_backend` | `cpu`, `cuda`, `rocm`, `vulkan`, `metal`, `mlx`, `xpu` |
| `status` | `stable`, `experimental`, `planned`, `deprecated` |

---

## 5. Podsekcje wariantu

Każdy `[[variant]]` MOŻE zawierać podsekcje. To, która jest wymagana, zależy od `deploy_mode`:

| `deploy_mode` | Wymagana podsekcja | Opcjonalna podsekcja |
|---------------|--------------------|-----------------------|
| `docker` | `[variant.build]` | `[variant.download]` |
| `python-bundle` | `[variant.build]` | – |
| `native` | `[variant.build]` | `[variant.download]` |
| `embedded` | `[variant.feature_flag]` | – |
| `external` | `[variant.detection]` | – |

### 5.1. `[variant.build]`

Definiuje sposób lokalnego buildu (Dockerfile, skrypt natywny, bundle Python).

| Pole | Typ | Wymagane | Opis |
|------|-----|----------|------|
| `context_path` | string | tak | Ścieżka względem `tentaflow-containers/`. Katalog MUSI istnieć. |
| `dockerfile` | string | nie | Tylko dla `deploy_mode = "docker"`. Default: `"Dockerfile"`. |
| `build_args` | table `string→string` | nie | Argumenty `--build-arg` (Docker) lub zmienne środowiskowe buildu. |
| `tags` | array of string | nie | Lokalne tagi obrazu / artefaktu. |

### 5.2. `[variant.download]`

Definiuje prebuilt artefakt do pobrania (głównie dla wariantu Pro).

| Pole | Typ | Wymagane | Opis |
|------|-----|----------|------|
| `image` | string | tak | Referencja OCI z tagiem. |
| `digest` | string `sha256:...` | warunkowo | 64 znaki hex. Wymagane gdy `enabled = true`. |
| `size_mb` | u64 | nie | Rozmiar do pobrania. |
| `license_required` | enum `pro`/`enterprise` | nie | Domyślnie `"pro"`. |
| `enabled` | bool | nie | Domyślnie `false`. |

### 5.3. `[variant.feature_flag]`

Wymagane gdy `deploy_mode = "embedded"`. Wskazuje Cargo feature flag aktywujący silnik
w binarce TentaFlow.

| Pole | Typ | Wymagane | Opis |
|------|-----|----------|------|
| `name` | string | tak | Nazwa Cargo feature, np. `"inference-llamacpp"`. |

### 5.4. `[variant.detection]`

Wymagane gdy `deploy_mode = "external"`. Opisuje jak wykryć zewnętrzny serwis.

| Pole | Typ | Wymagane | Opis |
|------|-----|----------|------|
| `binary` | string | tak | Nazwa binarki w `PATH`, np. `"ollama"`. |
| `endpoint` | URL | tak | Adres health check. |
| `health_path` | string | nie | Domyślnie `"/"`. |

---

## 6. Sekcja `[[model_preset]]` (opcjonalna)

Lista rekomendowanych modeli dla silnika. Zero lub więcej bloków na plik.

| Pole | Typ | Wymagane | Opis |
|------|-----|----------|------|
| `id` | string | tak | Unikalny w obrębie silnika. |
| `display_name` | string | tak | Nazwa wyświetlana. |
| `repo` | string | tak | Repo HuggingFace, URL GGUF, repo MLX itp. |
| `quantization` | string | nie | Np. `"Q4_K_M"`, `"fp16"`, `"int4"`. |
| `vram_gb_min` | u16 | nie | Minimalne VRAM dla tego modelu. |
| `recommended` | bool | nie | Domyślnie `false`. Jeden preset na silnik powinien być `true`. |

---

## 7. Reguły walidacji semantycznej

Build.rs sprawdza poniższe reguły dla każdego wariantu. Naruszenie = błąd kompilacji.

| # | Reguła |
|---|--------|
| 1 | `gpu_backend = "metal"` ⇒ `target_os ∈ {macos, ios}` |
| 2 | `gpu_backend = "mlx"` ⇒ `target_os ∈ {macos, ios}` AND `deploy_mode = "embedded"` |
| 3 | `gpu_backend = "cuda"` ⇒ `target_os ∈ {linux, windows}` |
| 4 | `gpu_backend = "rocm"` ⇒ `target_os = "linux"` |
| 5 | `gpu_backend = "xpu"` ⇒ `target_os ∈ {linux, windows}` |
| 6 | `deploy_mode = "docker"` ⇒ `target_os ∈ {linux, windows}` (macOS Docker bez GPU passthrough w v1) |
| 7 | `variant.build.context_path` musi istnieć na dysku (sprawdzane przez build.rs) |
| 8 | `variant.download.enabled = true` ⇒ `digest` podany i pasuje do `^sha256:[a-f0-9]{64}$` |
| 9 | Engine `id` unikalny globalnie (cross-file); variant `id` unikalny w obrębie engine |

---

## 8. Przykłady

### vLLM (Docker, prebuilt opcjonalnie)

```toml
[engine]
id = "vllm"
category = "llm"
name = "vLLM"
api = "openai-compatible"
default_port = 8000
version = "0.6.3"

[[variant]]
id = "linux-x64-cuda"
deploy_mode = "docker"
target_os = "linux"
target_arch = "x86_64"
gpu_backend = "cuda"
status = "stable"
vram_gb_min = 8

[variant.build]
context_path = "llm/docker/vllm"

[variant.download]
image = "ghcr.io/slyb00ts/tentaflow-pro/vllm:linux-x64-cuda-v0.6.3"
digest = "sha256:0000000000000000000000000000000000000000000000000000000000000000"
enabled = false
```

### llama.cpp (embedded przez Cargo feature)

```toml
[[variant]]
id = "embedded-metal"
deploy_mode = "embedded"
target_os = ["macos", "ios"]
target_arch = "aarch64"
gpu_backend = "metal"
status = "stable"
vram_gb_min = 4

[variant.feature_flag]
name = "inference-llamacpp"
```

### sherpa-onnx (native binarka)

```toml
[[variant]]
id = "native-linux-x64-cpu"
deploy_mode = "native"
target_os = "linux"
target_arch = "x86_64"
gpu_backend = "cpu"
status = "stable"

[variant.build]
context_path = "tts/native/sherpa-onnx"
```

---

## 9. Jak dodać nowy silnik

1. **Wybierz kategorię** — np. nowy silnik STT trafia do `tentaflow-containers/stt/`.
2. **Utwórz katalogi buildu** — w zależności od deploy mode:
   - Docker: `<kategoria>/docker/<id>/Dockerfile`
   - Python bundle: `<kategoria>/python/<id>/`
   - Native: `<kategoria>/native/<id>/build.sh`
3. **Stwórz manifest** — `<kategoria>/_services/<id>.toml` zgodny z tym schema.
4. **Zweryfikuj 9 reguł** — patrz sekcja 7.
5. **Uruchom `cargo build`** w `tentaflow-core/` — build.rs zwaliduje plik i wbuduje go do
   binarki. Błędy walidacji semantycznej zatrzymają kompilację.
6. **Uruchom skrypt buildu** — odpowiednio `build-containers.sh` lub `build-natives.sh`,
   żeby zbudować artefakty lokalnie.
7. **Sprawdź w GUI** — po starcie binarki w katalogu usług powinien pojawić się nowy silnik.

### Konwencje nazewnicze

- `engine.id`: kebab-case, krótki, zgodny z nazwą upstream (`vllm`, `llama-cpp`, `sherpa-onnx`).
- `variant.id`: kebab-case, format `<scope>-<arch>-<gpu>`, np. `linux-x64-cuda`,
  `embedded-metal`, `native-macos-arm64-metal`.
- `image` w `[variant.download]`: `ghcr.io/slyb00ts/tentaflow-pro/<engine>:<variant_id>-v<version>`.
