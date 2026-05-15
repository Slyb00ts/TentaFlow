# Manifest TOML — dokumentacja struktury `manifest.toml` dla addonow TentaFlow

Kazdy addon TentaFlow ma w katalogu glownym plik `manifest.toml` opisujacy
metadane, deklaracje storage, aliasow AI, gate'ow prawnych, narzedzi LLM,
regul sieciowych oraz custom komponentow UI. Parser TOML znajduje sie w
`src/addon/lifecycle.rs::parse_manifest_toml`; struktury Rust — w
`src/addon/mod.rs::AddonManifest` i `src/addon/manifest.rs`.

Ten dokument opisuje **wszystkie** sekcje (zarowno klasyczne, jak rozszerzenia
F1a wprowadzone dla TentaVision i kazdego dalszego enterprise addona).

---

## Konwencje

- Plik nazywa sie zawsze `manifest.toml` i lezy w katalogu glownym addona.
- Format: standardowy TOML 1.0 (parser `toml` v1.1+).
- Wszystkie identyfikatory (`id`, `name` w `[[vector_namespace]]`) sa **stringami
  case-sensitive**. Konwencja: kebab-case (np. `tentavision-yolo`,
  `d4-historical`, `tv-video-grid`).
- Zadne pole nie wymaga ucieczki polskich znakow — pliki sa traktowane jako UTF-8.
- Niezadeklarowane sekcje sa pomijane bez bledu (kompatybilnosc wsteczna).
- Rdzen odrzuca natomiast **legacy** sekcje (np. `[permissions]` z listami
  kategorii zamiast `[[permission]]`) — patrz `LEGACY_SECTIONS` w `lifecycle.rs`.

### Wersjonowanie SDK

Pole `[addon].sdk_version` (opcjonalne) to **semver range** (np. `">=0.2.0"`,
`"^0.3"`, `"0.2.0"`). Parser sprawdza, czy ciag jest poprawnym
`semver::VersionReq`. Faktyczne sprawdzenie kompatybilnosci z aktualna wersja
SDK (rejekcja install gdy mismatch) jest realizowane w M0.W2.

---

## `[addon]` — metadane podstawowe

Wymagane: `id`, `version`. Pozostale pola opcjonalne ale silnie zalecane.

```toml
[addon]
id = "my-addon"                    # wymagane, kebab-case, unikalne globalnie
name = "My Addon"                  # display name, fallback do id
version = "0.1.0"                  # semver, wymagane
description = "Krotki opis"
author = "TentaFlow"
license = "Apache-2.0"
icon = "shield"                    # id ikony sprite (np. "video", "meeting")
category = "communication"         # wolny string, kategoria w katalogu UI
keywords = ["foo","bar"]           # PL+EN, do semantic retrieval
runtime = "wasmtime"               # "wasmtime" (desktop) | "wasmi" (mobile)
platforms = ["linux","macos"]      # puste = wszystkie
wasm_file = "addon.wasm"           # sciezka wzgledna, default "addon.wasm"
sdk_version = ">=0.2.0"            # opcjonalny semver range, F1a
```

---

## `[application]` — rejestracja jako aplikacja w Apps launcher

Gdy obecna, addon pojawia sie w glownym menu GUI obok katalogu addonow.

```toml
[application]
entry_panel = "dashboard"   # id panelu UI startowego (wolany przez ui_render)
title = "My App"            # tytul pod ikona
icon = "video"              # opcjonalnie, default = [addon].icon
sort_order = 100            # mniejsza wartosc = wyzej w liscie
```

---

## `[service]` — tryb ciagly (background tick)

Aktywuje dedykowany tokio task, ktory wola `on_tick(timestamp_ms)` co
`tick_interval_ms`. Stop_addon anuluje task.

```toml
[service]
enabled = true                  # default true gdy sekcja istnieje
tick_interval_ms = 250          # None/0 = brak tickow, tylko on_event
tick_fuel_budget = 5000000      # paliwo na pojedynczy tick (default 5M)
tick_timeout_ms = 1000          # hard deadline w ms; watchdog → trap
```

---

## `[[permission]]` — granularne uprawnienia

Jedyne zrodlo prawdy dla uprawnien. Stare formaty (`[permissions]` z listami,
`[[addon_permissions]]`) sa odrzucane.

```toml
[[permission]]
id = "storage.read"                                  # konwencja host-function
display_name = "Read addon storage"
description = "Czyta zapisany counter."
risk = "low"                                         # low|medium|high|critical
gate = "d4-historical"                               # opcjonalne, ref do [[gate]]
```

---

## `[[oauth_provider]]` — providery OAuth

```toml
[[oauth_provider]]
id = "microsoft"
display_name = "Microsoft Identity Platform"
authorize_url = "https://login.microsoftonline.com/common/oauth2/v2.0/authorize"
token_url = "https://login.microsoftonline.com/common/oauth2/v2.0/token"
revoke_url = ""
scopes = ["offline_access", "OnlineMeetings.ReadWrite"]
mode = "individual"             # "global"|"individual"|"none"
pkce = true                     # default true
```

---

## `[[tool]]` — narzedzia LLM (tool calling)

```toml
[[tool]]
id = "join_meeting"
display_name = "Join a Teams meeting"
description = "Join a Teams meeting as an AI participant."
keywords = ["join","meeting"]

[[tool.parameter]]
name = "meeting_url"
param_type = "string"           # string|number|integer|boolean|array|object
description = "Teams meeting URL."
required = true
```

Parametry sa skladane przez parser w JSON Schema (`parameters_schema`).

---

## `[[network_rule]]` — TCP/UDP allowlist

```toml
[[network_rule]]
id = "graph"
protocol = "tcp"                # "tcp" | "udp"
host = "graph.microsoft.com"
port = 443
description = "Microsoft Graph API."
required = true                 # false = addon dziala bez tej reguly
```

Kazda regula wymaga zatwierdzenia admina (`approved=1`) przed pierwszym
uzyciem; niezatwierdzone reguly zwracaja `ABI_ERR_NETWORK_RULE_NOT_APPROVED`.

---

## `[visibility]` — widocznosc w GUI

```toml
[visibility]
admin_only = false              # true = tylko admin widzi w UI
show_in_catalog = true          # widocznosc w "Available apps"
default_groups = []
```

---

## `[resources]` — limity zasobow

```toml
[resources]
memory_mb = 256                 # limit pamieci WASM
fuel_limit = 10000000           # paliwo per wywolanie
storage_total_mb = 64           # MUSI byc > 0 dla store_set (no unlimited)
storage_value_mb = 1
http_requests_per_minute = 60
llm_tokens_per_minute = 200000
```

---

## `[storage]` — KV + SQL (F1a)

Deklaruje warstwy persistence addona. KV (per-addon namespace w hostowej
SQLite) jest domyslnie wlaczony; SQL wymaga jawnej deklaracji backendow.

```toml
[storage]
kv = true                            # default true
sql = true                           # default false
sql_backends = ["sqlite"]            # wymagane gdy sql=true; allowed: "sqlite","postgres"
sql_dialect = "ansi"                 # "ansi"|"sqlite"|"postgres"
migrations_dir = "migrations"        # katalog z plikami .sql, default "migrations"
encryption = "at-rest"               # "none"|"at-rest"
```

**Walidacja parsera:**
- `sql_dialect` musi byc w `{"ansi","sqlite","postgres"}`,
- `sql_backends[*]` musi byc w `{"sqlite","postgres"}`,
- `encryption` musi byc w `{"none","at-rest"}`,
- `sql=true` ⇒ `sql_backends` niepuste.

**Uwaga F1a:** backend `postgres` jest deklaratywnie dopuszczalny juz teraz, ale
runtime SQL host functions (`sql_exec`, `sql_query`, `sql_transaction`) obsluguje
tylko `sqlite` az do F8. Manifest z `sql_backends = ["postgres"]` parsuje, lecz
faktyczna instalacja na wezle bez Postgres zostanie odrzucona przez F1a installer.

---

## `[[alias]]` — aliasy AI (F1a)

Globalne aliasy AI deklarowane przez addon. Przy instalacji rdzen wywoluje
`create_or_reactivate_model_alias` w tabeli `model_aliases`. Jesli
`suggested_default` jest pusty, alias powstaje jako `is_active=0` i czeka, az
admin podepnie konkretny model/service (UI M16).

```toml
[[alias]]
id = "tentavision-yolo"
display_name = "Detektor obiektow (D1, D6)"
methods = ["detect", "track"]
suggested_default = "yolo11m-detector"      # moze byc pusty
visibility = "private"                       # private (default) | restricted | public
# allowed_consumers nie dotyczy private

[[alias]]
id = "tentavision-face-embed"
display_name = "Face embedding (D4)"
methods = ["embed"]
suggested_default = ""
gate = "d4-historical"                       # ref do [[gate]]
visibility = "restricted"
allowed_consumers = ["sharepoint-rag"]       # wymagane gdy visibility=restricted
```

**Walidacja:** duplikaty `id` w obrebie jednego manifestu sa bledem
(`Duplicate alias id: ...`). Gate `id` referowany przez `[[alias]].gate` nie
musi byc zaspokojony w momencie install — jesli nie jest, alias jest tworzony
nieaktywnie.

### Visibility i consumers (model dwukierunkowych uprawnien)

Kazdy alias zadeklarowany w manifescie ma owner = addon ktory go zadeklarowal.
Owner zawsze ma dostep do wlasnego aliasu w runtime. Inne addony moga uzyc
aliasu tylko jesli (a) zadeklaruja `[[uses_alias]]` z `id` aliasu oraz
(b) maja grant w core (auto lub manual w zaleznosci od `visibility`).

| Pole | Typ | Wymagane | Default | Opis |
|------|-----|----------|---------|------|
| `visibility` | string | nie | `"private"` | `private` (tylko owner), `restricted` (owner + addons z `allowed_consumers`), `public` (auto-grant dla kazdego addona ktory deklaruje `[[uses_alias]]`) |
| `allowed_consumers` | array<string> | tylko gdy `restricted` | `[]` | Lista `addon_id` ktorym admin a priori przyznaje dostep. Dla `restricted` musi byc niepusta. Dla `private`/`public` zabronione (parser blad). |

Aliasy NIE moga byc tworzone ani deaktywowane przez addon w runtime. Cykl
zycia aliasow to wylacznie hooki install/uninstall/upgrade w core
(`install_manifest_aliases` / `deactivate_aliases_owned_by_addon` / upgrade
diff w F1b/F2). Host functions widoczne dla addona to readonly:
`alias_get_v1`, `alias_list_owned_v1` — zob. `ADDON_HOST_FUNCTIONS.md`
sekcja "Aliases (readonly)".

---

## `[[uses_alias]]` — konsumowanie aliasow innych addonow (F1a)

Addon deklaruje, ze chce wywolywac alias ktorego ownerem jest inny addon
(albo alias `manual` utworzony przez admina). **Bez tej deklaracji** resolver
aliasow odrzuci `service_call` z `ABI_ERR_PERMISSION` (1) i wpisze do audit
`result=permission_denied`. Sama deklaracja nie wystarczy — admin/UI musi
przyznac grant (auto przy `visibility="public"` aliasu, manual przy
`"restricted"`, niemozliwy przy `"private"`).

```toml
[[uses_alias]]
id = "teams-stt"                  # alias_id z manifestu teams-bot
required = true                   # default false; true = addon nie wystartuje bez grantu
reason = "Reuse Teams meeting STT model for video subtitle generation."
```

| Pole | Typ | Wymagane | Default | Opis |
|------|-----|----------|---------|------|
| `id` | string | tak | — | `alias_id` aliasu, ktorego addon chce uzywac |
| `required` | bool | nie | `false` | `true` = bez grantu lifecycle install blokuje start addona; `false` = addon musi sam sobie poradzic z brakiem grantu (np. wylaczyc feature) |
| `reason` | string | nie | `""` | Czytelne uzasadnienie po angielsku — pokazywane adminowi w wizard install i w panelu grantow |

**Walidacja:** duplikaty `id` w `[[uses_alias]]` jednego manifestu = blad.

---

## `[[uses_model]]` — konsumowanie konkretnych modeli (F1a)

Bezposredni dostep do modelu (z pominieciem aliasu) — rzadko uzywane,
preferowana sciezka to alias + `service_call`. Owner modelu to **zawsze**
`system` (model wbudowany) albo `manual:<admin_id>` (model dodany recznie
przez admina) — addon **nigdy nie jest ownerem modelu**. Stad model ma tylko
dwa stany visibility: `restricted` i `public` (brak `private`).

```toml
[[uses_model]]
id = "yolo11m-detector"
required = false
reason = "Direct fallback when alias routing has no available target."
```

| Pole | Typ | Wymagane | Default | Opis |
|------|-----|----------|---------|------|
| `id` | string | tak | — | `model_id` z rejestru modeli core |
| `required` | bool | nie | `false` | Analogicznie do `[[uses_alias]]` |
| `reason` | string | nie | `""` | Uzasadnienie dla admina |

**Walidacja:** duplikaty `id` w `[[uses_model]]` = blad.

### Przyklad pelnego fragmentu manifestu (aliasy + uses_*)

```toml
[[alias]]
id = "teams-stt"
display_name = "Teams meeting STT"
methods = ["transcribe"]
suggested_default = "whisper-large-v3"
visibility = "restricted"
allowed_consumers = ["tentavision"]

[[alias]]
id = "teams-tts"
display_name = "Teams meeting TTS"
methods = ["synthesize"]
suggested_default = "elevenlabs-multilingual-v2"
visibility = "public"

[[uses_alias]]
id = "tentavision-face-embed"
required = false
reason = "Match meeting participant face to known contacts."

[[uses_model]]
id = "siglip-2-so400m"
required = false
reason = "Encode meeting slide screenshots for retrieval."
```

---

## `[[gate]]` — bramki claims-based (F1a)

Definicja gate'a prawno-biznesowego. Pola `required_claims` to **lista
wymagan**, ktore policy engine (F2) sprawdza przy probie uzycia chronionego
zasobu. F1a tylko parsuje i waliduje strukturalnie typy claimow — sama logika
sprawdzania klaimow zywie w policy engine F2.

```toml
[[gate]]
id = "d4-historical"
display_name = "Re-identyfikacja historyczna (D4)"
required_claims = [
  { type = "approval", subject = "dpia", status = "signed" },
  { type = "grant", scope = "biometric:historical", valid = true, has_expiry = true },
]
```

### `ClaimRequirement`

| Pole | Typ | Wymagane | Uzycie |
|------|-----|----------|--------|
| `type` | string | tak | "approval" / "grant" / "deployment_profile" / "consent" |
| `subject` | string | nie | dla `approval` (np. "dpia", "fria") |
| `scope` | string | nie | dla `grant`/`consent` (np. "biometric:historical") |
| `status` | string | nie | dla `approval` (np. "signed") |
| `value` | string | nie | konkretna wartosc |
| `oneof` | array<string> | nie | dla `deployment_profile` (np. `["lea","critical_infra"]`) |
| `valid` | bool | nie | wymusza aktualnie wazny claim |
| `has_expiry` | bool | nie | wymusza claim z hard expiry |

**Walidacja:** typy claimow musza byc w `{"approval","grant","deployment_profile","consent"}`,
`id` gate musi byc unikalny.

---

## `[[vector_namespace]]` — namespace wektorowe (F1a)

Addon deklaruje swoje przestrzenie embeddingow. F1a tylko parsuje — vector API
host functions (`vector_upsert`, `vector_search`, `vector_count`,
`vector_delete`) sa stubami do F1c/F2.

```toml
[[vector_namespace]]
name = "faces"
dimensions = 512
distance = "cosine"             # "cosine"|"euclidean"|"dot"
data_class = "C"                # "A"|"B"|"C" (klasa RODO)
gate = "d4-historical"          # opcjonalny gate
```

**Walidacja:** `distance` w `{"cosine","euclidean","dot"}`, `data_class` w
`{"A","B","C"}`, duplikaty `name` w obrebie addona = blad, `dimensions > 0`.

---

## `[[flow_template]]` — szablony Flow (F1a)

Lista szablonow Flow dostarczanych przez addon. **Opt-in install:** admin
moze je zaimportowac do flow-engine po instalacji addona (UI M15 krok 4/5).
Sam plik `.flow.json` powinien lezec w katalogu addona pod sciezka z `path`.

```toml
[[flow_template]]
id = "tv-realtime-adr"
display_name = "Real-time analiza ADR"
path = "flows/tv-realtime-adr.flow.json"
description = "frame → yolo → ocr → ADR validator → event"
```

**Walidacja:** duplikaty `id` = blad.

---

## `[[ui_component]]` — custom komponenty UI (F1a)

Addon moze dostarczyc wlasne web components do osadzania w panelach.

```toml
[[ui_component]]
id = "tv-video-grid"
display_name = "Grid kamer z bbox overlay"
slot = "main"                                           # "main"|"sidebar"|...
src = "components/tv-video-grid.js"
signature = "ed25519:<base64-signature-placeholder>"   # Ed25519 nad bundle JS
risk = "high"                                           # "low"|"medium"|"high"
```

### Model sandboxowania (planowany w F1c)

- `risk = "low"` lub `"medium"` → ladowane jako shadow DOM w tym samym originie
  co reszta UI.
- `risk = "high"` → ladowane w iframe sandbox z minimalnym `allow-scripts`.
  Komunikacja z hostem przez `postMessage` + ABI bridge.

### Generowanie sygnatury

Sygnatura jest tworzona przez narzedzia packaging (F1c) Ed25519 prywatnym
kluczem developera nad finalnym bundle JS i wstawiana do manifestu przed
spakowaniem `.tfaddon`. Walidacja regex: `^ed25519:[A-Za-z0-9+/]+=*$`. Parser
F1a akceptuje jawny placeholder `ed25519:<base64-signature-placeholder>` tylko
po to, by przyklad z `notes/tentavision-plan.md` parsowal sie 1:1.

**Walidacja:** duplikaty `id` = blad, `risk` w `{"low","medium","high"}`,
`signature` zgodny z regex.

---

## `[gpu]` — wymagania GPU (info-only, F1a)

Czysto informacyjna sekcja — rdzen nie blokuje instalacji na tej podstawie,
ale install wizard moze ostrzegac admina.

```toml
[gpu]
recommended_vram_mb = 12000
notes = "Dla pelnego profilu D2+D5 zalecane 24 GB; D4 wymaga osobnego node-a"
```

---

## `[config.schema]` — konfiguracja eksponowana adminowi

Adminowi wyswietlane w panelu addona jako form pole-per-klucz.

```toml
[config.schema]
bot_name = { type = "string", label = "Bot display name", default = "TentaFlow" }
silence_threshold_ms = { type = "number", label = "Silence threshold (ms)", default = 2000 }
maskowanie_default = { type = "bool", default = true }
```

---

## Walidacja przy instalacji

Lifecycle installer wykonuje kolejno:

1. **Parsowanie TOML** (struktura).
2. **Sprawdzenie `[addon].sdk_version`** jako semver range.
3. **Walidacja sekcji rozszerzonych** (`manifest::validate_manifest_extensions`):
   - duplikaty `id` w `[[alias]]` / `[[gate]]` / `[[vector_namespace]]` /
     `[[flow_template]]` / `[[ui_component]]`,
   - dozwolone enum-y (zob. tabela ponizej),
   - `storage.sql=true ⇒ sql_backends` niepuste,
   - `signature` `[[ui_component]]` w formacie `ed25519:<base64>`,
   - typy claimow w `[[gate]].required_claims[*].type`.
4. **Weryfikacja sygnatur UI componentow** (F1c packaging) — Ed25519 nad bundle JS.
5. **Sprawdzenie legacy sekcji** (`[permissions]`, `[[addon_permissions]]`)
   — jesli obecne, install rejected z bledem migracji.

| Pole | Dozwolone wartosci |
|------|--------------------|
| `storage.sql_dialect` | `ansi`, `sqlite`, `postgres` |
| `storage.sql_backends[*]` | `sqlite`, `postgres` |
| `storage.encryption` | `none`, `at-rest` |
| `vector_namespace.distance` | `cosine`, `euclidean`, `dot` |
| `vector_namespace.data_class` | `A`, `B`, `C` |
| `ui_component.risk` | `low`, `medium`, `high` |
| `gate.required_claims[*].type` | `approval`, `grant`, `deployment_profile`, `consent` |
| `alias.visibility` | `private`, `restricted`, `public` |

---

## Backward compatibility

Wszystkie nowe sekcje sa **opcjonalne**. Addony z istniejacymi manifestami
(`test-app-addon`, `teams-bot`, `embeddings-chunker`, `sharepoint-rag`,
`outlook`, `teams`, `malicious-addon`) parsuja sie bez modyfikacji i bez
uzyskania nowych funkcji — `m.aliases`, `m.gates`, `m.vector_namespaces`,
`m.flow_templates`, `m.ui_components` sa puste, `m.storage` i `m.gpu` to
`None`.

Test regresyjny `test_backward_compat_*` w `tests/addon_manifest_parsing.rs`
pilnuje, by zadna zmiana parsera nie psula istniejacych addonow.

---

## Pelny przyklad

Pelny manifest TentaVision (22 permissions, 6 aliases, 4 vector_namespaces,
3 flow_templates, 4 ui_components, 3 gates, [storage] + [gpu]) znajduje sie
w `notes/tentavision-plan.md` §5. Test
`test_parse_full_tentavision_manifest_ok` (w
`tests/addon_manifest_parsing.rs`) sprawdza, ze manifest parsuje i wszystkie
liczniki sa zgodne.

---

## Migration guide — istniejacy addon -> rozszerzenia F1a

Jesli addon dzis trzyma aliasy AI hard-coded w kodzie WASM (np. teams-bot ma
`TEAMS_BOT_ALIASES` na stale w `src/lib.rs`), migracja do `[[alias]]`:

1. W `manifest.toml` dodaj `[[alias]]` z odpowiednim `id`, `display_name`,
   `methods`, `suggested_default`.
2. Usun hard-coded stale z kodu addona — przy starcie addon woła
   `alias_get(id)` (M0.W2 stub, M1.W6 pelna implementacja) zamiast trzymac
   nazwy na stale.
3. Przy install rdzen automatycznie tworzy aliasy w `model_aliases`; admin
   wiaze je z konkretnymi modelami przez UI M16.

Podobnie dla `[[vector_namespace]]` — addony, ktore dzis zakladaja namespace
ad-hoc (np. wstawiajac do hostowego storage z kluczem `vectors:foo:...`),
migruja na deklaratywne namespace + `vector_*` host functions w F2.

Custom UI components (`[[ui_component]]`) wymagaja podpisu Ed25519, ktorego
generowanie zostanie zautomatyzowane w narzedziach packaging F1c
(`tentaflow-cli addon package --key dev.ed25519`).

---

## Narzedzie CLI: `tentaflow-cli addon validate`

Crate `tentaflow-cli` dostarcza polecenie `addon validate`, ktore wczytuje
manifest addonu, parsuje go, sprawdza spojnosc rozszerzen F1a (duplikaty id,
enumy, sygnatury Ed25519, semver), weryfikuje kompatybilnosc `sdk_version`
z rdzeniem oraz obecnosc plikow referowanych w manifescie.

### Uzycie

```
tentaflow-cli addon validate <sciezka>
```

`<sciezka>` moze wskazywac:
- katalog addonu zawierajacy `manifest.toml` (typowe uzycie podczas developmentu),
- bezposrednio na plik `manifest.toml` (uzywane w CI/fixturach).

### Co jest walidowane

| Krok | Zachowanie |
|------|------------|
| Parsowanie TOML | Wymusza kanoniczny format (`[[permission]]`, `[[tool]]`, ...) |
| Cross-sekcyjne id | Duplikat w `[[alias]]` / `[[gate]]` / `[[vector_namespace]]` / `[[flow_template]]` / `[[ui_component]]` -> bład |
| Enumy | `storage.sql_dialect`, `storage.encryption`, `distance`, `data_class`, `ui_component.risk`, `claim.type` |
| Sygnatury Ed25519 | `[[ui_component]].signature` musi pasowac do `^ed25519:<base64>$` lub byc placeholderem F1c |
| SDK version | `addon.sdk_version` (semver req) musi byc kompatybilne z `CORE_SDK_VERSION` |
| Pliki referowane | `flow_template.path`, `ui_component.src`, `storage.migrations_dir/*.sql` -> bład gdy brak |
| WASM binary | `addon.wasm_file` -> tylko ostrzezenie (build artifact w `target/wasm32-wasip1/release/`) |

### Przyklad — OK

```
$ tentaflow-cli addon validate ./addons-pro/tentavision
Walidacja addonu: ./addons-pro/tentavision/manifest.toml
Katalog: ./addons-pro/tentavision

OK  Manifest wczytany: tentavision v0.1.0
OK  Permissions: 22 zadeklarowane
OK  Aliasy AI: 6 zadeklarowane (2 z gate)
OK  Network rules: 3
OK  Gates: 3
OK  Vector namespaces: 4 (2 klasa C)
OK  Flow templates: 3
OK  UI components: 4
OK  SDK version: >=0.2.0 kompatybilne z core 0.2.0

Wynik: manifest poprawny. Mozna instalowac.
```

### Przyklad — bład

```
$ tentaflow-cli addon validate ./broken-addon
Walidacja addonu: ./broken-addon/manifest.toml
Katalog: ./broken-addon

OK  Manifest wczytany: broken v0.1.0
OK  Permissions: 0 zadeklarowane

BŁĄD Bład parsowania manifestu: Duplicate alias id: tentavision-yolo

Wynik: 1 bład — manifest niepoprawny, NIE instaluj.
```

### Exit codes

| Kod | Znaczenie |
|-----|-----------|
| 0 | Manifest poprawny (warnings nie blokuja) |
| 1 | Bład walidacji lub manifest nie istnieje |

### Integracja CI

Polecenie wraca kod 1 przy kazdym bledzie, wiec mozna spinac z `set -e`
lub `cargo test -p tentaflow-cli`. Crate `tentaflow-cli` ma testy integracyjne
(`tests/cli_addon_validate.rs`), ktore pokrywaja manifesty test-app-addon,
teams-bot oraz dwa zlamane fixtures z `tentaflow-core/tests/fixtures/`.
