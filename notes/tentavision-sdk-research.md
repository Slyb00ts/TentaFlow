# TentaVision — research nad SDK addonów TentaFlow

**Cel:** zrozumieć jak realnie zbudować TentaVision jako **addon-aplikację** (nie WASM-only podsystem, nie natywny silnik z addonem jako control plane). Konkretnie: jakie tryby pracy, jak modele AI, jak flow, jak external comms, czego brakuje.

Źródło: `/home/critix/repos/rust/TentaFlow/tentaflow-core/{src/addon, addon-sdk, addons, addons-pro}`.

---

## 1. Manifest addona (jedyny format: TOML)

Plik: `manifest.toml` w katalogu addona. Parser: `tentaflow-core/src/addon/mod.rs:58-114`.

Pełna struktura kanoniczna:

```toml
[addon]
id = "tentavision"
name = "TentaVision"
version = "0.1.0"
wasm_file = "tentavision.wasm"
runtime = "wasmtime"        # wasmtime | wasmi (mobile)
platforms = ["linux", "macos", "windows"]
icon = "video"
category = "surveillance"
keywords = ["video","cctv","analysis"]

[[permission]]
id = "service.call"
display_name = "Wywołaj usługi inference TentaFlow"
description = "Wywoływanie aliasów modeli (tentavision-yolo, tentavision-ocr, ...)"
risk = "medium"             # low | medium | high | critical

[[oauth_provider]]          # opcjonalne — np. integracje z VMS-em vendora
id = "unifi-protect"
authorize_url = "..."
token_url = "..."
scopes = []
mode = "individual"         # global | individual | none
pkce = true

[[network_rule]]            # KAŻDY outbound host musi być zadeklarowany
id = "camera-vlan"
protocol = "tcp"
host = "192.168.40.0/24"    # uwaga: dziś is_safe_ip blokuje private (zob. luka L5)
port = 554
required = true
description = "RTSP do kamer w VLAN 40"

[[tool]]                    # tryb TOOL — funkcje wywoływalne przez LLM/agentów
id = "search_attribute"
description = "Wyszukaj osoby/obiekty po opisie atrybutowym (D5)"
[[tool.parameter]]
name = "query"
param_type = "string"
required = true

[application]               # tryb APPLICATION — addon-aplikacja z własnym UI
entry_panel = "dashboard"
title = "TentaVision"
sort_order = 100

[service]                   # background tick (np. polling stanu, reagregacja)
enabled = true
tick_interval_ms = 1000
tick_fuel_budget = 5000000
tick_timeout_ms = 1000

[visibility]
admin_only = false
show_in_catalog = true

[resources]
memory_mb = 256
storage_total_mb = 64
http_requests_per_minute = 240

[config.schema]             # konfigurowalne przez admina w UI
default_flow_realtime = { type = "string", default = "flow-tentavision-realtime" }
default_flow_alarm    = { type = "string", default = "flow-tentavision-alarm" }
```

Brak (w SDK na dziś) sekcji takich jak: `[[service_alias]]` (model aliases), `[[flow_required]]` (wymagane Flow), `[gpu]` (deklaracja GPU). To są **luki do uzupełnienia w SDK** (zob. §10).

---

## 2. Trzy tryby pracy addona (mogą współistnieć)

Wszystkie trzy wskazują na ten sam eksport `on_request(input_json, output_buf)`. Różnica jest w **deklaracji w manifeście + routingu w core**.

### A. Tool dla LLM / agentów
Manifest: `[[tool]]`. Core dispatcher: `src/addon/tool_dispatch.rs:78-120`. Routing: `tool_call("search_attribute", args)` → `on_request({tool: "search_attribute", ...})`.

### B. Block w FlowBuilder
Manifest: brak — osobny plik **`blocks.json`** obok manifestu. Parser: `src/addon/flow_blocks.rs:82-110`. Block rejestruje się jako `addon.{addon_id}.{type}`, np. `addon.tentavision.adr_check`. Wywołanie z DAG: `on_request({tool: "block.adr_check", params})`.

### C. Aplikacja (UI launcher)
Manifest: `[application]`. Rendering deklaratywny — addon woła `ui_render(panel_id, json_tree)`, core cachuje w `AddonManager.ui_panels`, frontend pobiera przez `MessageBody::AddonUiPanelGetRequest` i renderuje przez `tf-*` web components. Drzewo komponentów: `ui_framework.rs:14-141` — Text, Input, Button, Select, Table, Card, Tabs, Image, List, Form, Divider, Progress, Code, Badge.

### Bonus: Service mode
Manifest: `[service] enabled=true`. Addon eksportuje `on_tick(ts_ms)` wołane co `tick_interval_ms`. Pod ABI: fuel limit + timeout. Używany np. do okresowego re-render dashboardu, polling stanu z service.

**TentaVision będzie wszystkim naraz:** Application (M1-M13 ekrany) + kilka Tools (search, check_adr, run_flow) + kilka FlowBuilder bloków (D1, D2, D3, D5) + Service tick (refresh dashboardu, re-aggregacja alarmów).

---

## 3. Service registry i aliasy modeli

**Kluczowy mechanizm:** addon nigdy nie wywołuje konkretnego modelu/serwisu. Woła **alias serwisu** przez:

```rust
// addon-sdk/sdk/src/lib.rs:679-705
service_request_call("tentavision-yolo", json_request)?;
```

Host function `service_request` (`addon/host_functions/service.rs:191-220`) szuka QUIC clienta w `service_manager` kolejno: LLM → Embedding → TTS → STT. Czyli **aliasing odbywa się infrastructure-side** (`core/services/runtime/quic_handle.rs`) — admin TentaFlow mapuje alias `tentavision-yolo` na konkretny serwis dockerowy na konkretnym nodzie (z fallbackami, load-balancingiem itd. — to już własność core, nie addona).

```rust
let model_request = ModelRequest {
    request_id: uuid::Uuid::new_v4().to_string(),
    payload: ModelPayload::Completion(CompletionPayload {
        model: service_name.to_string(),
        prompt: Some(request_json.to_string()),
        ...
    }),
};
let response = quic_client.send_request(model_request).await?;
```

**Wzorzec dla TentaVision:**
- Addon deklaruje w manifeście (sekcja propozycyjna, dziś jej nie ma) potrzebne aliasy: `tentavision-yolo`, `tentavision-ocr`, `tentavision-face-embed`, `tentavision-action`, `tentavision-vlm`, `tentavision-reid`.
- Przy instalacji admin TentaFlow widzi: "addon prosi o 6 aliasów modelowych" i ręcznie mapuje każdy na konkretny serwis na nodzie (lub używa autoroutingu).
- W runtime addon woła `service_request_call("tentavision-yolo", ...)` bez wiedzy o tym co jest pod spodem.
- Fallbacki, retry, load-balance, GPU scheduling — wszystko po stronie core/services.

**Rate limiting:** in-memory `AddonRateLimiter` per addon (`service.rs:31-187`).

---

## 4. Host functions dostępne dziś

Wszystkie zarejestrowane w `src/addon/host_functions/mod.rs:50-179`:

| Kategoria | Funkcje | Notatka |
|-----------|---------|---------|
| **Storage** | `storage_get/set/delete/list` | KV only, ~1 MB practical limit |
| **LLM** | `llm_generate`, `llm_generate_stream_start/next` | direct completion |
| **HTTP** | `http_request` | egress przez core proxy |
| **Events** | `event_publish`, `event_subscribe` | wewnątrz addon-bus |
| **UI** | `ui_render`, `ui_notify` | deklaratywny tree |
| **Secrets** | `secret_get`, `secret_set` | encrypted store |
| **Logs** | `log_info/warn/error` | structured |
| **User** | `user_get_current`, `user_check_permission` | role/perm check |
| **Tools** | `tool_register` | dynamic tool registration |
| **Network** | `net_connect/send/recv/close` | TCP/UDP przez core proxy (po SSRF check) |
| **Service** | `service_request` | QUIC do alias-serwisu (kluczowe dla TentaVision) |
| **OAuth** | `oauth_get_token` | per-provider |

**Każda** host function: permission check → `audit_log(action, resource, result)` → wywołanie. Bez wyjątków.

---

## 5. Permissions + enforcement

Granular per-permission, deny-by-default. Hierarchia (`addon/permissions.rs:76-150`):

1. Admin bypass (user w grupie admins)
2. User explicit allow/deny
3. Group explicit (any deny wygrywa, any allow→Granted, all inherit→next)
4. Addon defaults (z manifestu)
5. Fallback: **Denied**

Cache: `ArcSwap<HashMap>` (lock-free, COW), refresh co 5 min + po zmianie UI. `check()` nigdy nie trafia do DB.

**Instalacja addona:** UI checkbox per uprawnienie, kolorowane wg risk (low/medium/high/critical). Bez runtime escalation — wszystko z manifestu, admin akceptuje.

---

## 6. External communications — network rules

```toml
[[network_rule]]
id = "rtsp-vlan-40"
protocol = "tcp"
host = "192.168.40.0/24"
port = 554
description = "RTSP do kamer w VLAN 40"
required = true
```

Enforcement: `host_functions/network.rs:114-150`:
- Rule ID musi być w manifeście
- `is_safe_ip()` — **blokuje loopback, private (RFC 1918), link-local, metadata 169.254.169.254, fe80::/10**
- Addon musi mieć permission "network"
- Admin musi zatwierdzić rule per addon
- Max 10 concurrent connections per addon

**Problem dla TentaVision:** kamery są w **prywatnej** sieci. `is_safe_ip` je zablokuje. Musimy mieć override "allowed_private_networks" w manifeście lub przesunąć całość ingestu kamer do osobnego natywnego service (preferowane — i tak nie chcemy ramek w WASM).

---

## 7. UI integracja — deklaratywne tree

Addon NIE generuje HTML. Woła:
```rust
ui_render(panel_id, json_tree)
```

JSON tree = serializowane drzewo `UiComponent` (`ui_framework.rs:14-141`): Text/Input/Button/Select/Table/Card/Tabs/Image/List/Form/Divider/Progress/Code/Badge.

Frontend TentaFlow pobiera tree przez `AddonUiPanelGetRequest`, renderuje przez `tf-*` web components. Cache w `AddonManager.ui_panels` keyed by `(user_id, addon_id, panel_id)`.

**State**: addon utrzymuje state w `storage_set/get` — core nie persystuje UI state. Po `on_request("ui.main.submit_form")` addon ma sam zapisać i zrobić rerender.

**Limit:** **dziś brak custom web components addona** — tylko predefiniowanych 14 komponentów. Bogate mockupy TentaVision (heatmapa, polygon editor stref, video grid z bbox overlay) **wymagają rozszerzenia** (luka L8).

---

## 8. FlowBuilder integracja

Plik `blocks.json` obok manifestu. Każdy blok deklaruje: type, category, label, inputs, outputs. Parser rejestruje jako `addon.{addon_id}.{type}`. Wywołanie z DAG: `on_request({tool: "block.{type}", params})`.

**Dla TentaVision:** wystawiamy blocki:
- `addon.tentavision.adr_check` — input: frame_ref/camera_id, output: hazard_class+legibility+photo
- `addon.tentavision.search_attribute` — input: query+time_range, output: hits[]
- `addon.tentavision.luggage_check` — input: camera_id+zone, output: alerts[]
- `addon.tentavision.action_detect` — input: camera_id+window, output: actions[]

Admin/user buduje **flow** w FlowBuilder z tych bloków + standardowych (timer, http, branch). Addon TentaVision sam też potrafi **wywołać flow** przez core API (Tool: `addon.tentavision.run_flow(flow_id)` — to musi istnieć w host functions, do weryfikacji w SDK).

**TO JEST KLUCZOWY WZORZEC:** User wybiera w UI TentaVision **który flow** ma być wykonywany na alarmie, na cyklicznej analizie itd. Nie graf w UI addona — graf w FlowBuilder.

---

## 9. Audyt — co automatycznie, co jawnie

Automatycznie (host_functions/mod.rs:278-299): permission check, storage ops, http, service.call, event pub/sub, llm.generate, network ops, tool_register. Wszystko z `(user_id, addon_id, instance_id, action, resource_type, resource_id, result, error_message, action_hash)` → tabela `audit_log`.

**Brakuje** (luka L9): klasyfikacja ryzyka (D4 = class C). Dziś wszystko leci na jeden hash. Trzeba albo: konwencja w `resource_type` (`"class_c"`), albo rozszerzenie ABI (`audit_log_with_risk(action, risk_class)`).

---

## 10. Luki SDK blokujące TentaVision jako addon-aplikację

| # | Luka | Wpływ | Workaround / propozycja |
|---|------|-------|--------------------------|
| L1 | **Brak alias serwisów w manifeście** | Admin nie wie jakie aliasy mapować przy instalacji | Dodać `[[service_alias]]` sekcję: `id`, `display_name`, `kind`, `required`, `default_model` |
| L2 | **Brak deklaracji wymaganych Flow** | Admin musi ręcznie wiedzieć jakie flow utworzyć / podlinkować | Dodać `[[flow_required]]` (id, display_name, template_path) |
| L3 | **Brak object storage API** | Nie da się trzymać klipów 30 s, snapshot-ów, paczek dowodowych w addon storage (KV 1 MB) | Dodać `blob_put/get/delete` lub przekazać do natywnego service przez `service_request` |
| L4 | **Brak vector DB API** | Indeks atrybutów/embeddingów musi być extern | `service_request("tentavision-vector", ...)` dziś OK, ale bez SDK helpers |
| L5 | **is_safe_ip blokuje private** | RTSP do kamer w prywatnej sieci niemożliwe z poziomu addona | Dodać `[network_rule.allow_private_ranges]` (admin-confirmed), albo całkowicie wyłączyć ingest z addona |
| L6 | **Brak deklaracji GPU** | Addon nie wie czy GPU jest dostępne, nie może planować ciężkich flow | Dodać `[gpu]` info-only sekcję (`required`, `min_vram_mb`); same modele i tak na node-ach |
| L7 | **Brak real-time streaming / WebSocket** | `on_tick()` ograniczone do sekundowych intervalów; live grid wymaga push | Dodać `stream_subscribe(topic)` na event bus + push do UI |
| L8 | **Brak custom UI components** | Mockupy TentaVision (heatmapa, polygon editor, video grid+bbox) niemożliwe natively | Dodać `[ui.component]` rejestrację web component'u dostarczonego przez addon (lub permitować iframe slot) |
| L9 | **Brak klasyfikacji ryzyka w audycie** | D4 class C nie odróżnia się od D6 class A w audit log | Rozszerzyć `audit_log()` o pole `risk_class` enum |
| L10 | **Brak `run_flow` ABI / hooks** | Addon nie może wywołać Flow bezpośrednio | Dodać `flow_invoke(flow_id, input) → run_id` + `flow_status(run_id)` |
| L11 | **Brak `on_install` hooks z setup wizard** | Skomplikowana instalacja TentaVision (aliasy + flow + kamery) wymaga interaktywności | Dodać `on_install(ctx)` → addon zwraca multi-step wizard tree |
| L12 | **Brak permission proxy dla "działań klasy C"** | DPIA/FRIA gate musi być w UI addona, core nie wie | Dodać `[[capability_gate]]` w manifeście (capability id → wymagana podstawa prawna + grant lifecycle) |

---

## 11. Co to znaczy dla TentaVision

### Architektura, która REALNIE pasuje do SDK

```
┌─ TentaFlow core (host) ─────────────────────────────────────┐
│                                                              │
│  ┌─ TentaVision addon (WASM, application+tools+blocks) ──┐ │
│  │                                                         │ │
│  │  Application UI:  M1..M13 (przez ui_render tree)        │ │
│  │  Tools (LLM):     search_attribute, check_adr,          │ │
│  │                   confirm_alarm, run_flow,              │ │
│  │                   export_evidence                       │ │
│  │  Flow blocks:     adr_check, luggage_check,             │ │
│  │                   action_detect, search_attribute       │ │
│  │  Service tick:    refresh dashboardu, agregacja KPI     │ │
│  │                                                         │ │
│  │  storage_get/set: konfiguracja, profile, mappingi      │ │
│  │  service_request: tentavision-yolo, -ocr, -face, ...   │ │
│  │  flow_invoke:     wywołanie wybranego Flow             │ │
│  │  event_publish:   nowe alarmy → core event bus          │ │
│  │  network_rule:    rzadko (np. webhook callback)         │ │
│  └────────────────────────────────────────────────────────┘ │
│                                                              │
│  ┌─ Core: service registry, flow runner, audit, perms ────┐ │
│  │  Routuje service_request("tentavision-yolo") do        │ │
│  │  Docker service na konkretnym node-zie                 │ │
│  └────────────────────────────────────────────────────────┘ │
└──────────────────────────────────────────────────────────────┘
            │ QUIC          │ QUIC          │ QUIC
            ▼               ▼               ▼
   ┌─ Service A ──┐  ┌─ Service B ──┐  ┌─ Service C ──┐
   │ camera-     │  │ yolo-server  │  │ ocr-server   │
   │ ingest      │  │ (Docker)     │  │ (Docker)     │
   │ (Docker)    │  │ GPU          │  │ GPU          │
   │ RTSP→frames │  │              │  │              │
   └─────────────┘  └──────────────┘  └──────────────┘
```

Czyli:

1. **Camera ingest** (RTSP, ONVIF, vendor connectors) = **natywny service** na nodzie (Docker), zarejestrowany w TentaFlow jako serwis typu `camera-source` z aliasem `tentavision-cam-{id}`. Wystawia QUIC API: `next_frame_ref()`, `snapshot()`, `subscribe_events()`.
2. **Decode + frame bus** = w tym samym service (lub osobnym), bo ramki nie mogą lecieć przez WASM boundary.
3. **Modele AI** = osobne Docker services per model: `yolo11m-detector`, `ppocrv5-ocr`, `siglip2-embed`, itd. Admin TentaFlow je rejestruje na nodach z konkretnym hardware.
4. **Aliasy** = mapowanie `tentavision-yolo` → `yolo11m-detector@node-gpu-A` (z fallback `@node-gpu-B`). To core router.
5. **Pipeline** = **Flow w FlowBuilder**, nie graf w UI addona. Bloki to: `addon.tentavision.adr_check`, `service.call.tentavision-yolo`, `service.call.tentavision-ocr`, `service.call.recording-segmenter`, `event.emit.alarm`. User komponuje + zapisuje.
6. **TentaVision addon UI** = wybór profili, kamer, stref, harmonogramów; **wybór którego Flow użyć** dla real-time vs alarm vs eksport; oglądanie wyników; legal gates D4.
7. **Recording / evidence packaging** = osobny service `tentavision-recording` + `tentavision-evidence` (HSM + TSA — to też service, addon je woła).
8. **Vector index / search** = service `tentavision-vector` (Qdrant lub embedded). Addon woła przez `service_request`.

### Mockupy do przeprojektowania / dodania

Z powyższego wynika, że mockup M4 ("Profile analityczne — builder grafu operatorów") jest **niezgodny z modelem TentaFlow**. Powinien być zastąpiony przez:

- **M4 (nowy):** "Profile analityczne — wybór Flow per cel". Profil = lista (cel, FlowId, harmonogram, kamery). Builder grafu = otwiera FlowBuilder (poza addonem).
- **M14 (nowy):** "Aliasy modeli i serwisów" — admin mapuje wymagane aliasy addona na konkretne service'y na nodach. Status: ok/missing/degraded, latencja, fallback chain.
- **M15 (nowy):** "Instalacja addona — wizard" — wybór profilu prawnego (już mamy w M13) + akceptacja permissions + mapowanie aliasów + import szablonów Flow + utworzenie network rules.

### Manifest TentaVision (draft v0.1)

```toml
[addon]
id = "tentavision"
name = "TentaVision"
version = "0.1.0"
wasm_file = "tentavision.wasm"
runtime = "wasmtime"
platforms = ["linux"]
icon = "video"
category = "surveillance"
keywords = ["video","cctv","analysis","adr","baggage","behavior"]

# === Tryb 1: aplikacja ============================================
[application]
entry_panel = "dashboard"
title = "TentaVision"
sort_order = 100

# === Tryb 2: background tick =====================================
[service]
enabled = true
tick_interval_ms = 1000
tick_fuel_budget = 5000000
tick_timeout_ms = 2000

# === Uprawnienia ================================================
[[permission]]
id = "service.call"
display_name = "Wywołaj serwisy inference TentaFlow"
description = "Aliasy modeli: yolo, ocr, face-embed, action, vlm, reid"
risk = "medium"

[[permission]]
id = "flow.invoke"
display_name = "Wywołaj Flow z FlowBuilder"
description = "Uruchamianie wskazanych przez użytkownika flow do analizy obrazu"
risk = "medium"

[[permission]]
id = "storage.read"
risk = "low"
[[permission]]
id = "storage.write"
risk = "low"

[[permission]]
id = "event.publish"
display_name = "Publikuj eventy alarmowe"
risk = "medium"

[[permission]]
id = "secret.read"
risk = "high"

# === Aliasy serwisów wymagane (sekcja propozycyjna — L1) ==========
[[service_alias]]
id = "tentavision-yolo"
display_name = "Detektor obiektów (YOLO11/RF-DETR)"
kind = "vision-detection"
required = true

[[service_alias]]
id = "tentavision-ocr"
display_name = "OCR ADR / tablic"
kind = "vision-ocr"
required = true

[[service_alias]]
id = "tentavision-action"
display_name = "Klasyfikator akcji (VideoMAE V2 / InternVideo2)"
kind = "vision-action"
required = false

[[service_alias]]
id = "tentavision-vlm"
display_name = "VLM dla atrybutów (SigLIP2 / EVA-CLIP)"
kind = "vision-embedding"
required = false

[[service_alias]]
id = "tentavision-face-embed"
display_name = "Face embedding (AdaFace) — D4 only"
kind = "vision-biometric"
required = false
risk_class = "C"           # uwidacznia że to klasa C — gate w UI

[[service_alias]]
id = "tentavision-reid"
display_name = "Person re-id (TransReID) — D4 only"
kind = "vision-biometric"
required = false
risk_class = "C"

[[service_alias]]
id = "tentavision-recording"
display_name = "Recording segmenter (ring buffer)"
kind = "storage-recording"
required = true

[[service_alias]]
id = "tentavision-vector"
display_name = "Vector index (Qdrant / faiss)"
kind = "vector-db"
required = true

[[service_alias]]
id = "tentavision-evidence"
display_name = "Evidence packager (HSM/TSA signing)"
kind = "evidence"
required = true

# === Flow wymagane (sekcja propozycyjna — L2) =====================
[[flow_required]]
id = "tv-realtime"
display_name = "Real-time analysis"
template = "flows/realtime.flow.json"

[[flow_required]]
id = "tv-alarm"
display_name = "Alarm enrichment"
template = "flows/alarm.flow.json"

[[flow_required]]
id = "tv-evidence-export"
display_name = "Evidence export"
template = "flows/evidence.flow.json"

# === Tools (LLM/agentów) ==========================================
[[tool]]
id = "search_attribute"
description = "Wyszukaj osoby/obiekty po opisie atrybutowym (D5)"
[[tool.parameter]]
name = "query"
param_type = "string"
required = true
[[tool.parameter]]
name = "time_window_minutes"
param_type = "integer"
required = false

[[tool]]
id = "check_adr"
description = "Sprawdź czytelność tablicy ADR (D1)"
[[tool.parameter]]
name = "camera_id"
param_type = "string"
required = true

[[tool]]
id = "confirm_alarm"
description = "Potwierdź lub odrzuć alarm"
[[tool.parameter]]
name = "alarm_id"
param_type = "string"
required = true
[[tool.parameter]]
name = "verdict"
param_type = "string"   # "confirm" | "reject" | "escalate"
required = true

# === Capability gates (sekcja propozycyjna — L12) =================
[[capability_gate]]
id = "d4-realtime"
display_name = "D4 real-time re-identyfikacja"
requires = ["dpia", "fria", "legal_grant", "deployment_profile_lea_or_critical"]

[[capability_gate]]
id = "d4-historical"
display_name = "D4 wyszukiwanie historyczne"
requires = ["dpia", "legal_grant"]

# === GPU info-only (sekcja propozycyjna — L6) =====================
[gpu]
recommended_vram_mb = 12000
notes = "Dla pełnego profilu D2+D5 zalecane 24 GB; D4 wymaga osobnego node"

# === Network — minimalne (większość ruchu wewn. przez service_request)
[[network_rule]]
id = "webhook-callback"
protocol = "tcp"
host = "*.tentaflow.local"
port = 443
description = "Webhook callback do flow-engine TentaFlow"
required = false

# === Resources ===================================================
[resources]
memory_mb = 256
storage_total_mb = 128
http_requests_per_minute = 60

# === UI custom components (sekcja propozycyjna — L8) =============
[[ui_component]]
id = "tv-video-grid"
slot = "main"
src = "components/tv-video-grid.js"

[[ui_component]]
id = "tv-zone-editor"
slot = "main"
src = "components/tv-zone-editor.js"

[[ui_component]]
id = "tv-heatmap"
slot = "main"
src = "components/tv-heatmap.js"

# === Konfiguracja eksponowana adminowi ===========================
[config.schema]
default_flow_realtime  = { type = "string", default = "tv-realtime" }
default_flow_alarm     = { type = "string", default = "tv-alarm" }
default_flow_export    = { type = "string", default = "tv-evidence-export" }
deployment_profile     = { type = "string", default = "commercial" }
worm_bucket            = { type = "string", default = "" }
tsa_url                = { type = "string", default = "https://freetsa.org/tsr" }
```

---

## 12. Wnioski

1. **TentaVision = addon-aplikacja**, ale ciężar wykonawczy **MUSI** żyć w usługach Dockerowych zarejestrowanych w TentaFlow. Addon jest control plane + UI + tools + flow blocks.
2. Modele AI dostępne dla addona **tylko** przez `service_request_call(alias, json)`. Aliasy mapowane przez admina przy instalacji. To wymusza dodanie sekcji `[[service_alias]]` w SDK.
3. Pipeline analizy obrazu = **Flow w FlowBuilder**, nie wewnętrzny graf w UI addona. Addon tylko wybiera który Flow użyć dla danego celu.
4. Każda komunikacja zewnętrzna addona przechodzi przez core (network rules + SSRF check). Kamery w prywatnej sieci są problemem — preferujemy je obsługiwać w natywnym service `camera-ingest`.
5. Audit jest automatyczny ale bez klasyfikacji ryzyka — dla D4 (klasa C) trzeba rozszerzyć.
6. Bogate UI (video grid, polygon editor, heatmapa) wymaga rozszerzenia SDK o custom web components — dziś tylko 14 predefiniowanych.
7. Lista 12 luk SDK do uzupełnienia jest oddzielnym dokumentem `tentavision-addon-api-gaps.md` (do utworzenia).
8. Mockup M4 trzeba przepisać (Profile → wybór Flow, nie wewnętrzny graf operatorów). Dodać M14 (Aliasy serwisów) i M15 (Wizard instalacji).
