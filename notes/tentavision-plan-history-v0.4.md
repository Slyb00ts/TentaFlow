# TentaVision — plan addona analizy obrazu z kamer (v0.4 — konsolidacja)

**Wersja:** v0.4 · spójna konsolidacja iteracji v0.1–v0.3.4
**Forma:** addon-aplikacja TentaFlow (tryby: application + service tick + tools + flow blocks)
**Deployment:** on-premise (serwery klienta), opcjonalnie multi-node
**Stylistyka UI:** Manrope, paleta TentaFlow indigo/violet, komponenty tf-* + custom web components z sygnaturą
**Historia poprzednich wersji:** `tentavision-plan-history-v0.1-v0.3.4.md`
**Powiązane dokumenty:**
- `tentavision-sdk-research.md` — analiza addon SDK TentaFlow z cytatami kodu
- mockupy: `~/.gstack/projects/Slyb00ts-TentaFlow/designs/tentavision-v1/` (16 ekranów M1–M16)

---

## Spis treści

1. Wizja i scope
2. Architektura systemowa
3. Komponenty (czego TentaVision faktycznie się składa)
4. Mockupy M1–M16 — opis realizacji
5. Manifest TentaVision (finalny draft)
6. SDK API gaps — co musi być dopisane do TentaFlow
7. Modele AI i ich rola
8. Connectory kamer
9. Dane: SQL schema, vector namespaces, recording
10. RODO / EU AI Act / claims-based gates
11. Bezpieczeństwo
12. Wydajność i SLO
13. Dataset strategy
14. Evaluation harness
15. Roadmap implementacyjny F0–F10
16. Otwarte pytania i decyzje do podjęcia
17. Glossary

---

## 1. Wizja i scope

TentaVision to addon-aplikacja TentaFlow analizująca strumienie wideo z kamer IP w czasie rzeczywistym i z historii. Jest pełnoprawną aplikacją osadzoną w shellu TentaFlow — admin TentaFlow ją instaluje z marketplace, addon żyje wewnątrz globalnej nawigacji (Addons → TentaVision), korzysta z core API TentaFlow (storage, aliasy AI, kamery, streaming, recording, evidence) i nie dotyka bezpośrednio żadnego hardware ani sieci kamerowej.

### 1.1 Sześć domen analitycznych

| ID | Domena | Tryb | Krytyczność | Klasa RODO/AI Act |
|----|--------|------|-------------|---------------------|
| D1 | ADR — kontrola tablic chemicznych na cysternach (czytelność, klasa zagrożenia) | real-time przy bramach/dokach | wysoka | A (bezosobowe) |
| D2 | Anomalie zachowań — upadek, agresja, wandalizm, broń | real-time | krytyczna | B (anonimowa sylwetka) |
| D3 | Pozostawiony bagaż — peron, lotnisko, hala | real-time + post-event | krytyczna | A/B |
| D4 | Re-identyfikacja osób (twarz + person re-id, gait eksperymentalny) | post-event pod legal gate; real-time tylko dla LEA | wysoka | **C — AI Act high-risk, Art. 5 prohibited zone** |
| D5 | Wyszukiwanie po atrybutach (CLIP/SigLIP), tablice rejestracyjne, marki/kolory aut | post-event | średnia | B |
| D6 | Generic object detection — audyt, statystyki, custom klasy | real-time, opt-in per kamera | niska | A |

### 1.2 Zasady projektowe

- **Profil analityczny per kamera + harmonogram dzień/noc** — nie wszystko leci jednocześnie
- **Pipeline = Flow w FlowBuilder TentaFlow**, nie wewnętrzny graf w UI addona; TentaVision dostarcza bloki + szablony Flow
- **Modele AI = aliasy w globalnej tabeli `model_aliases` TentaFlow**; addon tylko deklaruje potrzebne aliasy, admin konfiguruje target i fallbacki w systemowym UI
- **Operator I linii widzi maskowane twarze**; analyst/DPO/uprawniony mogą zażądać unmask z aktywnym claim
- **D4 (klasa C) jest twardo zablokowane** dopóki addon nie dostanie kompletu claims (DPIA/FRIA/LegalGrant/deployment profile)
- **Audit hash-chain do WORM** dla każdego query klasy B/C, każdego unmask, każdego eksportu

### 1.3 Co NIE jest scope TentaVision

- ❌ Bezpośredni dostęp do RTSP/ONVIF/Protect z addona — to obsługa core (camera ingest)
- ❌ Własna inferencja na GPU — wszystko przez aliasy AI rozwiązywane przez router TentaFlow
- ❌ Trzymanie ramek wideo lub klipów w storage addona — recording API zwraca opaque `clip_ref`
- ❌ Konfiguracja mapowania alias → konkretny model — to robi admin w globalnym UI TentaFlow → Serwisy → Aliasy
- ❌ Generyczny vector / blob / message-queue / FS storage poza tym co wystawia core SDK
- ❌ Wybór polityki retencji recording globalnie — addon deklaruje klasy danych, core enforce wedle config

---

## 2. Architektura systemowa

### 2.1 Warstwy

```
┌─ TENTAFLOW CORE (host) ──────────────────────────────────────────────────┐
│                                                                            │
│  ┌─ Addon WASM: TentaVision ────────────────────────────────────────┐   │
│  │  application UI (M1..M13)  · ui_render tree                       │   │
│  │  service tick (250 ms)     · drenaż streamów + agregacja          │   │
│  │  tools (LLM)               · search/check/confirm/run_flow        │   │
│  │  flow blocks               · D1/D2/D3/D5 jako bloki w FlowBuilder │   │
│  │                                                                     │   │
│  │  ↓ host functions SDK (każda permission-check + audit-log)         │   │
│  │  storage_get/set (KV) · sql_exec/query · vector_upsert/search      │   │
│  │  camera_add/list/snapshot · stream_subscribe/next                  │   │
│  │  recording_save_segment/get · evidence_sign · service_call         │   │
│  │  alias_create/deactivate/get · flow_invoke · secret_set/get        │   │
│  └────────────────────────────────────────────────────────────────────┘   │
│                                                                            │
│  ┌─ Core moduły (TentaFlow services) ───────────────────────────────┐   │
│  │  service registry         · routuje service_call po aliasach     │   │
│  │  router (model_aliases)   · resolves alias → target+fallback     │   │
│  │  camera-ingest module     · RTSP/ONVIF/Protect → frame_refs      │   │
│  │  recording manager        · ring-buffer, retencja, signed_urls   │   │
│  │  vector store             · embedded HNSW per addon/namespace    │   │
│  │  evidence signer          · HSM (Yubikey HSM2) + TSA RFC 3161    │   │
│  │  flow runner              · wywołuje Flow z FlowBuilder          │   │
│  │  audit chain              · append-only + hash-chain + WORM      │   │
│  │  policy/claims engine     · gates dla operacji klasy C           │   │
│  └────────────────────────────────────────────────────────────────────┘   │
│                                                                            │
│  ┌─ UI TentaFlow www (globalne) ─────────────────────────────────────┐   │
│  │  /addons/tentavision/*    · 13 ekranów addona (M1..M13)           │   │
│  │  /services/aliases        · M16 — globalny UI aliasów             │   │
│  │  /addons/*/install        · M15 wizard instalacji per addon       │   │
│  │  /addons                  · marketplace + lista zainstalowanych   │   │
│  └────────────────────────────────────────────────────────────────────┘   │
└────────────────────────────────────────────────────────────────────────────┘
            │ QUIC                │ QUIC                  │ QUIC
            ▼                     ▼                       ▼
   ┌─ Service A ──┐      ┌─ Service B ──┐         ┌─ Service C ──┐
   │ yolo11m-     │      │ ppocrv5-ocr   │         │ siglip2-vit  │
   │ detector     │      │ (Docker, GPU) │         │ (Docker, GPU)│
   │ (Docker, GPU)│      └───────────────┘         └──────────────┘
   └──────────────┘
   ...zarejestrowane services rozdzielone po nodach klastra
```

### 2.2 Ścieżki danych (przepływy)

**Klatka kamery → detekcja → alarm:**
1. `tentaflow-camera-ingest` (moduł core) trzyma sesję RTSP z kamerą C-01
2. Addon przy starcie woła `stream_subscribe(Camera{id: C-01, sample_fps: 5}, filter)` → dostaje `StreamId`
3. W `on_tick` (co 250 ms) addon woła `stream_next(id, 200 ms timeout)` → dostaje `Frame{camera_id, ts, frame_ref, sequence}`
4. Addon wywołuje `service_call("tentavision-yolo", "detect", {frame_ref, classes: ["truck","tanker"]})` — router rozwiązuje alias na `yolo11m-detector @ node-gpu-A`, serwis pobiera ramkę po `frame_ref` z core, zwraca bboxy
5. Dla ADR (D1): addon woła `service_call("tentavision-ocr", "recognize_cropped", {frame_ref, region: bbox})`, dostaje tekst tablicy
6. Walidacja w addonie (lokalna tabela ADR 2025 jako asset bundled w wasm)
7. Jeśli alarm: `sql_exec("INSERT INTO alarms ...")` lokalnie + `event_publish("alarm.created", payload)` do bus + opcjonalnie `flow_invoke("tv-alarm-enrich", {alarm_id})`

**Wyszukiwanie historyczne (D5):**
1. User w M6 wpisuje "czerwona czapka, okulary"
2. Addon woła `service_call("tentavision-vlm", "embed", {text: query})` → wektor 768D
3. Addon woła `vector_search("attributes", vector, k=30, filter)` → top-30 hits z metadata (camera_id, ts, frame_ref)
4. Addon woła `recording_save_snapshot(camera_id, ts)` dla pokazania miniatury (lub czyta z cache jeśli wcześniej zapisana)
5. Render results grid (M6)

**Eksport dowodowy:**
1. User w M11 wybiera alarm/szereg alarmów + wpisuje sygnaturę sprawy
2. Addon zbiera `clip_refs` z `recordings_meta` (lokalna SQL tabela), klatki snapshotów, metadane
3. Addon woła `evidence_sign({clip_refs, snapshots, manifest_json: {legal_grant_id, case_no, ...}})` → core składa ZIP, podpisuje HSM, dołącza TSA timestamp, opcjonalnie anchor BTC
4. Wynik: `SignedPackage{id, bundle_url, signature, timestamp_token, chain_hash}`
5. Addon wysyła link `bundle_url` adminowi/uprawnionemu odbiorcy + zapisuje w `evidence_exports` SQL

### 2.3 Aliasy AI — model przetwarzania

Aliasy żyją w globalnej tabeli `model_aliases` TentaFlow (potwierdzone w kodzie: `tentaflow-core/src/db/migrations.rs:225`). Schemat: `(alias UNIQUE, target_model, is_active, fallback_targets, strategy)`.

| Rola | Co robi |
|------|---------|
| **Addon** | Przy aktywacji woła `alias_create(spec)` dla każdego aliasu z manifestu (`[[alias]]`). `spec.suggested_default` jest zapisywany jako `target_model` (lub pusty). Aliasy z `gate` są tworzone z `is_active = 0`. Przy dezaktywacji addona — `alias_deactivate(id)`. |
| **Admin TentaFlow** | W globalnym UI **Serwisy → Aliasy** (M16) widzi wszystkie aliasy (z różnych addonów + manualne), edytuje `target_model`, `fallback_targets` (chain z drag-to-reorder), `strategy` (`first_available` / `round_robin` / `weighted`), `is_active`. |
| **Router (core)** | Przy każdym `service_call(alias, method, payload)` rozwiązuje alias zgodnie ze strategią. Dla `first_available`: próbuje primary, przy `down` → kolejny z fallback chain. Zwraca `ServiceResponse{payload, executed_by, duration_ms, fallback_used}`. |

Wzorzec potwierdzony w teams-bot: `tentaflow-core/src/addon/mod.rs:1890` — `TEAMS_BOT_ALIASES = [("teams-stt","stt-1"), ("teams-tts","tts-1"), ("teams-vision-face","") ...]`. Aliasy z pustym targetem są wypełniane przez auto_bind po deployu silnika lub manualnie w M16.

---

## 3. Komponenty TentaVision

### 3.1 Addon WASM (TentaVision package)

Bundle dostarczany jako paczka:
- `manifest.toml` — pełna deklaracja (§5)
- `tentavision.wasm` — kompilowany kod Rust → WASM (wasm32-wasip1)
- `migrations/*.sql` — schema bazy (uruchamiane przy install/upgrade, kolejność leksykograficzna)
- `flows/*.flow.json` — szablony Flow do opt-in importu w FlowBuilder
- `components/*.js` — custom web components (sandboxowane, signature Ed25519)
- `assets/adr-table-2025.json` — tabela klas ADR jako bundled asset (regex + lookup)
- `assets/legal/*.md` — szablony klauzul informacyjnych, DPIA/FRIA, PL/EN/UA
- `LICENSE`, `README.md`

Tryby pracy (wszystkie aktywne równolegle):

| Tryb | Manifest | Cel |
|------|----------|-----|
| **Application** | `[application] entry_panel = "dashboard"` | 13 ekranów M1..M13 w shellu TentaFlow |
| **Service tick** | `[service] tick_interval_ms = 250` | Drenaż stream_next, agregacja KPI, refresh M1 |
| **Tools (LLM)** | `[[tool]]` × 5 | `search_attribute`, `check_adr`, `confirm_alarm`, `run_flow`, `export_evidence` — wywoływalne przez agentów LLM TentaFlow |
| **Flow blocks** | `blocks.json` | `addon.tentavision.adr_check`, `addon.tentavision.luggage_check`, `addon.tentavision.action_detect`, `addon.tentavision.search_attribute` |

### 3.2 Serwisy AI (Docker, GPU) — zewnętrzne wobec addona

Zarejestrowane w TentaFlow service registry (przez admina, niezależnie od TentaVision). Każdy serwis ma:
- nazwę (np. `yolo11m-detector`, `ppocrv5-ocr`, `siglip2-vit-l14`)
- node binding (na którym węźle żyje)
- capabilities (typ inference, akceptowane wejście, GPU requirements)
- QUIC endpoint
- health probe

Lista serwisów potrzebnych TentaVision (admin musi mieć zarejestrowane przed instalacją lub w trakcie):

| Service nazwa (przykład) | Funkcja | Hardware |
|---------------------------|---------|----------|
| yolo11m-detector | D1, D6 — detekcja obiektów | GPU 8+GB |
| yolo11s-detector | fallback dla yolo11m | GPU 4+GB lub CPU |
| ppocrv5-ocr | D1 — OCR tablic ADR + rejestracji | GPU 4+GB |
| parseq-adr | fallback OCR dla cropped ADR | GPU |
| videomae-v2-rwf2k | D2 — klasyfikator akcji (agresja, upadek) | GPU 12+GB |
| weapons-yolo-fine-tune | D2 — detekcja broni | GPU |
| siglip2-vit-l14 | D5 — VLM dla atrybutów + indeksowania | GPU 8+GB |
| adaface-r100 | D4 — face embedding (klasa C) | GPU 4+GB |
| transreid | D4 — person re-id (klasa C) | GPU |
| lprnet-pl | D5 — tablice rejestracyjne | GPU lub CPU |

### 3.3 Core moduły TentaFlow (nowe, do dopisania)

| Moduł | Wystawia API | Status w core |
|-------|--------------|---------------|
| Camera ingest | `camera_*`, RTSP/ONVIF/Protect connectors, frame_refs | **brak** — do dopisania (API-5) |
| Streaming bus | `stream_subscribe/next/close`, FrameRef opaque | **brak** (API-6) |
| Recording manager | `recording_save_segment/get_url/purge`, ring-buffer per kamera | **brak** (API-8) |
| Vector store | `vector_upsert/search/delete`, embedded HNSW | **brak** (API-7) |
| Evidence signer | `evidence_sign/verify/anchor`, HSM + TSA | **brak** (API-9) |
| SQL backend | `sql_exec/query/transaction`, per-addon SQLite + opcjonalny PG | **brak** (API-3, API-4) |
| Alias mgmt (rozszerzenie) | `alias_create/deactivate/get` jako WASM ABI | **częściowo** — repository istnieje, brak host functions (API-1a) |
| Policy/claims engine | `claim_add`, `gate_check`, audit z `risk_class` | **brak** (API-15) |
| Flow invoke | `flow_invoke(flow_id, input)`, `flow_status(run_id)` | **brak** (API-11) |
| Custom UI components | `[[ui_component]]` z Ed25519 + iframe sandbox | **brak** (API-10) |
| Per-addon FS sandbox | `~/.tentaflow/addons/<id>/` infrastructure | **brak** (API-4b) |

### 3.4 UI TentaFlow www (rozszerzenia)

| Lokalizacja UI | Co dodajemy | Mockup |
|----------------|--------------|--------|
| `/addons/tentavision/dashboard` | 12 zakładek (Dashboard / Live / Kamery / Profile / Alarmy / Wyszukiwarka / Re-ID / Modele / Bindings / Strefy / Audyt / Eksport / Ustawienia) | M1..M14 |
| `/addons/{addon-id}/install` | 6-krokowy wizard generyczny dla każdego addona | M15 |
| `/services/aliases` | Globalny UI aliasów (poza addonem, istniejąca sekcja Services) | M16 |
| `/addons/tentavision/onboarding` | Profil prawny (PL/EU AI Act) | M13 |

---

## 4. Mockupy M1–M16 — opis realizacji

Format dla każdego ekranu:
- **Cel** — jaki problem użytkownika rozwiązuje
- **Persona** — kto używa
- **Storyboard** — co user widzi i co może zrobić
- **Pod spodem** — jakie SDK API są wywoływane, skąd dane, jakie tabele SQL, jakie aliasy, jakie permissions
- **Klasa danych** — A/B/C wg RODO

### M1 — Dashboard

**Cel:** szybki przegląd zdrowia całego systemu na jednym ekranie. Analyst widzi: ile kamer online, ile aktywnych detektorów, ile alarmów 24h, GPU/latencja p95, heatmapa aktywności.

**Persona:** operator I linii (viewer), analyst, admin TentaVision.

**Storyboard:**
- Detail-header: nazwa "TentaVision", chip statusu runtime, badges (kamery 22/24, detektory 8, alarmów 3 critical, GPU 68%)
- Action buttons: Odśwież / Eksport raportu / Live view
- Tabs-bar (12 zakładek) z aktywną "Dashboard"
- 4 KPI tiles: Aktywne kamery, Aktywne detektory, Alarmy 24h, GPU/latencja p95
- Sekcja "Ostatnie alarmy" (lista 4 ostatnich z miniaturkami, klikalne)
- Sekcja "Stan runtime" (tabela: frame bus throughput, queue depth, drop rate 1h, VRAM, modele załadowane, clock-sync dryf, audit→WORM status, eval harness status)
- Heatmapa aktywności 24h × kamera (skala kolorów: niska → wysoka)
- Segmented control 1h/6h/24h/7d (kontroluje zakres KPI i heatmapy)

**Pod spodem:**
- **Źródło KPI:**
  - "Aktywne kamery" = `camera_list({status: online}).count` (Camera API)
  - "Aktywne detektory" = liczba aktywnych aliasów z `alias_list_owned()` + agregacja per-profil z `sql_query("SELECT COUNT(DISTINCT detector) FROM profiles WHERE active=1")`
  - "Alarmy 24h" = `sql_query("SELECT COUNT(*) FROM alarms WHERE ts > ?", [now - 24h])` plus critical count z `WHERE status = 'critical'`
  - "GPU / latencja" = agregat z metadanych `service_call(...).duration_ms` ostatnich N wywołań, trzymane w KV (`storage_get("perf:gpu_util")`)
- **Lista ostatnich alarmów:** `sql_query("SELECT id, ts, camera_id, detector, status, confidence, clip_ref FROM alarms ORDER BY ts DESC LIMIT 4")`. Miniaturka pobierana przez `recording_get_url(clip_ref, ttl_sec=600)` — addon przekazuje URL do `<img>` w `ui_render` tree
- **Stan runtime:** kombinacja KV (`storage_get("runtime:*")` zapisywane przez service tick), audit query (`storage_get("audit:last_worm_sync")`), eval harness (`storage_get("eval:last_run")`)
- **Heatmapa:** `sql_query("SELECT camera_id, FLOOR(ts/3600) AS hour, COUNT(*) FROM alarms WHERE ts BETWEEN ? AND ? GROUP BY camera_id, hour", [now-24h, now])` → rendering jako grid CSS przez `ui_render` (UiComponent::Grid lub custom component)
- **Service tick:** co 250 ms odświeża KPI, zapisuje agregaty do KV. Pełny re-render co 5s lub gdy alarm wpłynie do bus
- **Permissions wymagane:** `camera.read`, `sql.read`, `recording.read`, `storage.read`

**Klasa danych:** mix A/B (alarmy klasy A — D1/D6; B — D2/D3/D5). Brak klasy C na tym ekranie (D4 alarmy mają specjalne traktowanie — pokazywane jako "alarm Re-ID" bez treści, klikalność tylko dla uprawnionych).

---

### M2 — Live view (grid kamer)

**Cel:** monitoring real-time wielu kamer z overlay analizy. Operator widzi co dzieje się w obiekcie, gdzie są alarmy w tej chwili.

**Persona:** operator I linii (viewer), analyst.

**Storyboard:**
- Detail-header z liczbą aktywnych kamer i statusem maskowania
- Tabs-bar z aktywną "Live view"
- Sticky toolbar nad gridem: toggle Overlay (bounding boxes / etykiety / pose skeletons / strefy / mask faces), chipy aktywnych detektorów per kamera
- Segmented 1/4/9/16 do wyboru układu gridu
- Grid kafelków video (aspect 16:9) z:
  - Overlay top: nazwa kamery, REC indicator (pulsujący)
  - Overlay bottom: chip status detektora (success/warning/danger), FPS, zegar
  - Bounding box rysowane na obrazie (success=zielony, warning=żółty, danger=czerwony) z etykietą "klasa · confidence"
- Kamery offline pokazane z czerwonym overlay i czasem od utraty heartbeat
- Tip pod gridem: shift+klik = porównanie dwóch kamer

**Pod spodem:**
- **Strumień ramek:** dla każdej widocznej kamery addon trzyma aktywną subskrypcję `stream_subscribe(Camera{id, sample_fps: 5}, filter)` → `StreamId`. W `on_tick` woła `stream_next(id, timeout=200ms)` po wszystkich aktywnych streamach
- **Render obrazu:** `FrameRef` jest opaque — addon nie ma bajtów. W `ui_render` tree addon używa `UiComponent::Image{src}` gdzie `src` to URL signed `recording_get_url(frame_ref_as_clip_ref, ttl=60s)` lub specjalny endpoint `frame_url(frame_ref)` (API-6 rozszerzenie)
- **Overlay bbox:** addon dostaje z subskrypcji `StreamMessage::Detection{frame_ref, boxes: Vec<Bbox>}` jeśli subskrypcja includuje detector profile (nie surowe ramki). Lub osobny subskrypcja `stream_subscribe(DetectorEvents{profile_id})` per profil
- **Maskowanie twarzy:** wykonuje się **po stronie core** w camera-ingest lub w dedykowanym frame-postprocess module na podstawie `data_class` profilu + roli usera. Addon nie ma kontroli nad surowym obrazem
- **Custom web component:** `tv-video-grid` (signed Ed25519, zarejestrowany w manifeście `[[ui_component]]`). Otrzymuje przez postMessage bridge dane: `{cameras: [{id, frame_url, bboxes, status, fps}]}`. Sandbox iframe `sandbox="allow-scripts"` (bez allow-same-origin)
- **Toggle overlay:** przechowuje stan w KV `storage_set("ui:m2:overlay", json)`, persystencja per user
- **Permissions:** `camera.read`, `stream.subscribe`, `recording.read`

**Klasa danych:** A/B w zależności od profilu kamery. C nie pojawia się na live view bez aktywnego claim.

---

### M3 — Kamery — lista & wizard "dodaj kamerę"

**Cel:** zarządzanie listą kamer w deployment + dodawanie nowych przez 4-krokowy wizard z auto-discovery.

**Persona:** admin TentaVision.

**Storyboard:**
- Detail-header: 24 kamer skonfigurowanych, 22 online, 2 offline, 5 z ostrzeżeniami
- Search box (filtrowanie po nazwie/IP/vendorze)
- Button "+ Dodaj kamerę" otwiera wizard
- Filter tabs: Wszystkie / Online / Offline / Ostrzeżenia / Niepowiązane
- Tabela kamer: Nazwa | Vendor / Protokół | Adres | Status (pill) | Profil analityczny (chip) | FPS | Diagnostyka (chip warning/success) | menu kontekstowe (⋯)
- Wizard 4-krokowy (M3 pokazuje krok 2):
  - Krok 1: Odkrywanie (ONVIF WS-Discovery + mDNS + ARP scan)
  - Krok 2: Wybór z listy znalezionych + poświadczenia (user/hasło → zapisywane w secret vault)
  - Krok 3: Podgląd + kalibracja (test stream, ustawienie FPS, walidacja capabilities)
  - Krok 4: Wybór profilu analitycznego z dostępnych

**Pod spodem:**
- **Lista kamer:** `camera_list()` (Camera API) → tabela CameraInfo. Lokalna replika w SQL `cameras` table dla szybkiego query po atrybutach + cache miniaturki
- **Auto-discovery:** specjalna metoda `camera_discover(network_hint, timeout)` w Camera API — core wykonuje WS-Discovery + mDNS + ARP, zwraca listę kandydatów z metadata (vendor, MAC, protokół, możliwości)
- **Test connection:** `camera_test_connection({vendor, url, credentials_ref})` — zwraca capabilities + snapshot URL
- **Zapis kamery:** `camera_add(spec)` → `CameraId` + lokalny `sql_exec("INSERT INTO cameras ...")` z `clip_ref_to_ownership`. **Poświadczenia idą przez `secret_set("cam-<id>-creds", encrypted_blob)` zanim spec.credentials_secret_ref zostanie przekazany — addon nigdy nie widzi plaintext**
- **Diagnostyka:** background job (service tick) wywołuje `camera_health(id)` co 30 s, wynik do `sql_exec("UPDATE cameras SET last_seen=?, health_flags=? WHERE id=?")` i KV cache. Flagi: `backpressure` (z monitoringu queue), `image_dark` (z camera quality probe), `clock_drift` (z PTP/NTP)
- **Wariancje firmware:** core camera-ingest moduł ma listę quirks (Hikvision firmware 5.7.x ma ONVIF off, Dahua region-locked, itd.) — przy `camera_add` raportuje wykryte issue, UI pokazuje warning + propozycję fix
- **Permissions:** `camera.manage`, `secret.write` (dla credentials)

**Klasa danych:** A (sama kamera = metadata, bez treści wideo). Credentials = secrets (klasa B/C w zależności od polityki org).

---

### M4 — Profile analityczne

**Cel:** profil = `{cel, Flow, kamery, harmonogram, retencja, quick params}`. Builder NIE jest grafem w UI addona — pełna edycja DAG to otwarcie FlowBuilder. W TentaVision UI: wybór Flow + szybkie parametry (thresholdy, FPS, klasy obiektów).

**Persona:** analyst, admin TentaVision.

**Storyboard:**
- Detail-header: nazwa profilu "ADR-brama", risk class A, badges (Flow: tv-realtime-adr, 3 kamery, 24/7)
- Action buttons: Test na nagraniu / Zapisz
- Tabs-bar z aktywną "Profile"
- Sub-tabs profilu: Graf operatorów / Harmonogram / Strefy / Akcje&reguły / Kamery / Historia zmian (aktywna "Graf operatorów")
- Lewa kolumna: karta Flow z metadanymi (nazwa, liczba bloków, capabilities, wersja, autor, edytowane kiedy) + dropdown "Zmień Flow" (lista Flow z FlowBuilder filtrowanych do tych co używają capabilities pasujących do TentaVision)
- Sekcja "Quick params" — slidery: legibility threshold, FPS sampling, latency budget, min. wielkość bbox, klasy obiektów (chip multi-select)
- Prawa kolumna: konfiguracja (nazwa, cel z dropdownem, harmonogram, retencja, tier QoS), lista kamer w profilu, sekcja "Capabilities tego Flow" (readonly mapping capability → service)
- Tabela "Profile w deployment": Nazwa | Flow | Klasa | Kamery | Harmonogram | Aktywny (toggle)

**Pod spodem:**
- **Profile w SQL:** tabela `profiles(id, name, flow_id, schedule, retention, data_class, active, quick_params_json)`
- **Lista Flow z FlowBuilder:** `flow_list(filter: {requires_capabilities: ["vision.detect","vision.ocr"]})` (nowa funkcja w Flow API — `flow_list` jako rozszerzenie do API-11). Filtrowanie po deklarowanych capabilities Flow
- **Quick params jako override do Flow inputs:** addon przy `flow_invoke(flow_id, params)` przekazuje quick_params jako część params; Flow ma inputs że są overridable per-run
- **Capabilities mapping per Flow:** `flow_get(flow_id).capabilities` zwraca jakie capabilities Flow używa (czyta z aliasów wewnątrz Flow definition). UI pokazuje per capability jaki alias jest faktycznie pod spodem
- **Zmiana Flow w profilu:** dropdown writes do `profiles.flow_id`. Restart subskrypcji streamów dla kamer w profilu
- **Wywoływanie Flow:** background service tick lub on-frame event `flow_invoke("tv-realtime-adr", {frame_ref, camera_id, profile_id})` → `RunId`. Flow asynchronicznie wykonuje + emituje event przez `flow_status(run_id)`
- **Test na nagraniu:** specjalna metoda `flow_invoke_with_input(flow_id, recorded_clip_ref)` która zamiast live frame podaje historyczny clip
- **Permissions:** `sql.read/write`, `flow.invoke`, `camera.read`

**Klasa danych:** A (profile to konfiguracja, nie dane). Quick params + Flow reference w bazie addona.

---

### M5 — Centrum alarmów

**Cel:** real-time feed alarmów z filtrami + szczegółowa karta alarmu z klipem 30 s, klatkami i workflow potwierdzania (confirm / reject / escalate).

**Persona:** operator I linii (potwierdza/odrzuca), analyst (workflow), admin.

**Storyboard:**
- Detail-header: liczba critical/warning (chip), informacja kto operator
- Action buttons: Filtry / Wycisz dźwięk
- Tabs-bar z aktywną "Alarmy" + badge liczby niepotwierdzonych
- Split layout:
  - **Lewa kolumna (380px):** feed alarmów z filter tabs (Niepotwierdzone 23 / Wszystkie 147 / Zamknięte). Karta alarmu compact: thumb, tytuł (D2 podejrzenie agresji), meta (kamera, czas, severity chip)
  - **Prawa kolumna (1fr):** karta alarmu szczegółowa: header (severity chip, conf, camera, ts), embedded klip video 30 s (play overlay), timeline klatek (10 thumbnailów: −2s, −1s, EVT, +1s, +2s, +3s, +4s, +5s, +6s, +7s), grid 2 kolumn:
    - Metadane (detektor, confidence, strefa, track IDs, maskowanie, model version)
    - Workflow: decyzja (Potwierdź / Fałszywy / Eskaluj), notatka operatora (textarea), toggle akcji powiązanych (Wyślij SMS, Zachowaj klip, Eksportuj paczkę dowodową)

**Pod spodem:**
- **Feed alarmów (left):** subskrypcja `stream_subscribe(EventBus{topic: "alarm.created"}, filter)` → push do feed. Plus inicjalny load z SQL: `sql_query("SELECT * FROM alarms WHERE status='pending' ORDER BY ts DESC LIMIT 50")`
- **Detail karta:** `sql_query_one("SELECT * FROM alarms WHERE id=?", [alarm_id])`. Klip 30 s: `recording_save_segment(camera_id, alarm.ts - 5s, alarm.ts + 25s)` zwraca `ClipRef` (cache w `alarms.clip_ref`). Embedded video: `recording_get_url(clip_ref, ttl_sec=3600)` → `<video src>` w UI
- **Timeline klatek:** `recording_save_snapshot(camera_id, alarm.ts + offset)` dla każdej z 10 pozycji → SnapshotRef → URL
- **Workflow:** klik "Potwierdź" → `sql_exec("UPDATE alarms SET status='confirmed', operator_id=?, notes=?, confirmed_at=? WHERE id=?")` + `event_publish("alarm.confirmed", {alarm_id})` + audit log z `risk_class` (B dla D2/D3, A dla D1/D6)
- **Eskalacja:** podobnie + `flow_invoke("tv-alarm-escalate", {alarm_id})` który może wysłać do dispatchera, do supervisora, wezwać patrol
- **Permissions:** `sql.read/write`, `recording.read/save`, `stream.subscribe`, `event.publish`

**Klasa danych:** A/B/C wg detektora. Klipy klasy C (D4) wymagają unmask + claim.

---

### M6 — Wyszukiwarka historyczna

**Cel:** post-event search po treści: tekst semantyczny ("czerwona czapka okulary"), atrybut formularzowy (kolor + ubranie + wzrost), podobieństwo (upload zdjęcia → top-K), tablica rejestracyjna.

**Persona:** analyst.

**Storyboard:**
- Detail-header: indeks 10.4M klatek, embeddings SigLIP2, retencja 14 dni, zakres ostatnie 7 dni
- Action buttons: Eksport wyników
- Tabs-bar z aktywną "Wyszukiwarka"
- Sekcja "search modes" — 4 karty (active=Tekst): Tekst (semantyczne) / Atrybut (formularz) / Podobieństwo (zdjęcie) / Tablica rejestracyjna
- Formularz query (dla "Tekst"): textarea + chipy aktywnego embedding (SigLIP2, cosine, top-K=30)
- Form-rows: Kamery (multi-select), Zakres czasu (datetime range), Klasy obiektów (filter chips)
- Status bar: czas wyszukania, ile klatek przeszukano
- Button "Szukaj"
- Wyniki: section-card z segmented "Grid / Timeline / Mapa" + 5-kolumnowy grid result-card (thumb z bbox, label, info: kamera + ts + score)
- Footer warning RODO: każde wyszukanie po atrybutach D5 zapisywane w audit; D4 (face) wymaga aktywnego LegalGrant → link do M7

**Pod spodem:**
- **Indeksowanie (background):** dla każdej kamery z aktywnym profilem D5/D6, service tick periodycznie woła `service_call("tentavision-vlm", "embed", {frame_ref})` co N sekund → wektor + zapisuje w `vector_upsert("attributes", [{id: "{camera_id}:{ts}", vector, metadata: {camera_id, ts, frame_ref}}])`
- **Query tekstowe:** `service_call("tentavision-vlm", "embed", {text: query})` → wektor 768D → `vector_search("attributes", query_vec, k=30, filter: {camera_id IN [...], ts BETWEEN ...})` → top-30 hits
- **Query po obrazie (podobieństwo):** user uploaduje zdjęcie → `service_call("tentavision-vlm", "embed", {image_bytes})` → vector_search jak wyżej
- **Query po tablicy:** `service_call("tentavision-yolo", "detect", {filter: license_plate})` + walidacja format PL/EU lokalnie, lub osobny alias `tentavision-anpr` → search w `vector_namespace("plates")`
- **Render wyników:** dla każdego hit thumbnail przez `recording_save_snapshot(camera_id, ts)` + bbox overlay na grid (custom component `tv-results-grid`)
- **Audit:** każde wyszukanie → `sql_exec("INSERT INTO search_audit (user_id, query_type, query_text, time_window, hits_count, ts)")` + audit_log z risk_class B
- **Eksport wyników:** klik "Eksport" → `evidence_sign({clip_refs: hits.map(...), manifest_json: {query, results}})` → paczka dla raportu (klasa B, nie pełna evidence)
- **Permissions:** `vector.read`, `service.call`, `recording.read`, `sql.write` (audit)

**Klasa danych:** B (atrybuty). Faces/persons (klasa C) dostępne wyłącznie przez M7.

---

### M7 — Re-ID (D4) — pod legal gate

**Cel:** wyszukiwanie po twarzy / sylwetce osoby. **Twardo zablokowane** dopóki nie spełnione wszystkie claims w gate `d4-historical` lub `d4-realtime`. Pokazuje checklist gate + workflow uzupełnienia + (po unlock) tabelę indeksu re-id z TTL i audit każdego query.

**Persona:** analyst-lea, DPO, supervisor uprawniony.

**Storyboard:**
- Detail-header: czerwone obramowanie, big-ico gradient czerwono-fioletowy, risk class C, chip "zablokowany"
- Tabs-bar z aktywną "Re-ID" (z ikoną kłódki)
- Gate modal (centralny, 720px): big icon, tytuł "Moduł Re-ID jest zablokowany"
- Wyjaśnienie prawne (RODO + AI Act Art. 5 + Annex III)
- Check-list 6 warunków:
  - DPIA podpisana ✓ (DPO)
  - FRIA — szkic 60% (warning)
  - LegalGrant z authority — brak (blocked)
  - Profil deployment ≠ "Komercja prywatna" — aktualny "Komercja" (blocked)
  - Hash-chain audit log + WORM ✓
  - Post-market monitoring ✓
- Per check: "Otwórz wniosek" / "Zmień profil" / "Wnioskuj o LegalGrant"
- Po unblock: tabela indeksu `subj-XXXX` z legal_grant_id, sygnatura sprawy, authority, expiry, operator

**Pod spodem:**
- **Gate check (przy wejściu na ekran):** `gate_check("d4-historical")` → zwraca status każdego required claim. Core czyta z `policy_claims` tabeli (claim_id, type, subject, status, expiry, scope)
- **DPIA/FRIA flow:** osobny generator w M10, tutaj tylko link out
- **LegalGrant request:** klik "Wnioskuj" → wypełniony formularz (authority, case_no, scope, expiry) → submit → `claim_add({type:"grant", scope:"biometric:historical", ...})` + workflow zatwierdzenia (DPO + supervisor)
- **Zmiana profilu deployment:** klik "Zmień profil" → link do M12 Ustawienia → sekcja profil prawny → wymaga podpisu DPO
- **Po unlock — indeks:** `sql_query("SELECT subject_id, authority, case_no, expiry, scope, dpo_signature FROM legal_grants WHERE expiry > NOW() AND scope LIKE 'biometric:%' AND is_active=1")`. Każdy wpis = możliwy subject do query
- **Query D4 face:** `service_call("tentavision-face-embed", "embed", {image_bytes_of_target})` → wektor → `vector_search("faces", vec, k=10, filter: {legal_grant_id: ?})`. **Tylko po sprawdzeniu gate_check za każdym razem** (audyt każdego query z risk_class C)
- **Każdy query D4 = wpis w hash-chain audit z risk_class=C**, eksport do WORM bucket
- **Permissions:** `vector.read` (faces/persons namespace), `service.call` (face-embed alias musi być is_active=1), `audit.write_classC` (specjalne uprawnienie)
- **Right to be forgotten:** osobne narzędzie (M10) usuwa subject z `vector_delete("faces", [subject_id])` + audit

**Klasa danych:** C (twardo).

---

### M8 — Modele i runtime

**Cel:** stan wszystkich modeli AI używanych przez TentaVision (przez aliasy), VRAM budget, benchmark per kamera, możliwość rollback modelu, upload własnego ONNX z sanity test.

**Persona:** admin TentaVision, ML engineer.

**Storyboard:**
- Detail-header: 6 modeli załadowanych, GPU RTX 4070 12GB, CUDA 12.4
- Action buttons: Uruchom benchmark / Upload ONNX
- Tabs-bar z aktywną "Modele"
- Sekcja VRAM budget (kolorowy bar 12GB rozłożony per model + decode surfaces + wolne)
- Tabela modeli: Model | Domena (chip) | Wersja / hash | Licencja | VRAM | Throughput | Status (live/shadow/blocked) | rollback button
- Sekcja benchmark per kamera: tabela kamera + profil + FPS (osiągany/target z bar) + latency p95 + GPU share

**Pod spodem:**
- **Lista modeli:** `service_list()` (Service Registry API — nowa, lub rozszerzenie istniejącej z core) zwraca wszystkie zarejestrowane services + ich metadata. Addon filtruje do tych co są używane przez aliasy TentaVision (`alias_list_owned().flat_map(get_target_chain)`)
- **VRAM info:** każdy service przy QUIC handshake raportuje VRAM usage (model weights + engine + buffers). Core agreguje per node, addon czyta przez `node_resources_get(node_id)`
- **Benchmark:** klik "Uruchom benchmark" → CLI command albo background job `tentavision_bench(profile, duration=60s)`. Wynik do KV i tabela
- **Rollback:** klik rollback button → `service_call(target_model, "rollback", {to_version})`. Service ma archive poprzednich N wersji weights, rollback w < 60s. Nie addon dostarcza modeli — tylko zleca rollback service'owi
- **Upload ONNX:** modal "Upload" → file picker → upload do core przez `model_upload(file_bytes, manifest)` → core sprawdza signature/sanity → rejestruje jako nowy service → admin może zmapować w aliasie (M16)
- **Permissions:** `service.read`, `service.call` (dla rollback)

**Klasa danych:** A (metadata modeli). Nie ma wrażliwych danych.

---

### M9 — Strefy, harmonogramy, reguły

**Cel:** rysowanie stref (polygon, linia przekroczenia, strefa wykluczeń) na obrazie kamery, kalendarz tygodniowy profili, reguły kompozytowe (AND/OR detektorów).

**Persona:** analyst, admin.

**Storyboard:**
- Detail-header: 4 strefy zdefiniowane, 2 linie, 1 zone exclude
- Action buttons: Harmonogram / Nowa strefa
- Tabs-bar z aktywną "Strefy"
- Layout dwie kolumny:
  - Lewa: widok kamery C-07 (aspect 16:9) z narysowanymi polygons (purple = zone, red dashed = exclude, blue line = przekroczenie). Każdy polygon ma label + draggable vertices. Toolbar: segmented Polygon/Linia/Exclude. Buttons: Dodaj punkt / Edycja / Usuń
  - Prawa: lista stref kamery z kolorowymi dotami i nazwami
- Sekcja harmonogram tygodniowy: grid 5h-rows × 7-days, kolory profile (day/night/off)
- Sekcja reguły kompozytowe: tabela z wyrażeniami (`D3.luggage(unowned>90s) AND zone.peron AND not zone.lawka`), akcją, toggle aktywności

**Pod spodem:**
- **Strefy w SQL:** tabela `zones(id, camera_id, name, type, polygon_json, color, used_by_detectors)`. Polygon JSON to lista `[{x_pct, y_pct}]` (procent obrazu — niezależne od resolution)
- **Edytor polygonów (custom component):** `tv-zone-editor` (signed). Otrzymuje obraz kamery przez `camera_snapshot(id)` + initial polygons przez postMessage. Edycja → emit save event → addon woła `sql_exec("UPDATE zones SET polygon_json=? WHERE id=?")`. Custom component sandboxed iframe
- **Harmonogram:** `sql_query("SELECT * FROM schedules WHERE camera_id=?")`. Tygodniowy grid renderowany jako UiComponent::Grid z kolorowymi cellami
- **Reguły kompozytowe:** tabela `rules(id, name, expression, action, active)`. Expression to mini-DSL parsowany w addonie. Eval w trakcie obsługi detection events: `eval_rule(expr, context: {detections, zones, time, schedule})`
- **Permissions:** `sql.read/write`, `camera.read` (snapshot)

**Klasa danych:** A.

---

### M10 — Audyt + RODO

**Cel:** hash-chain audit log (append-only + WORM externalizacja), retencja per klasa, generator DPIA / FRIA / klauzul informacyjnych.

**Persona:** DPO, admin, audytor.

**Storyboard:**
- Detail-header: hash-chain OK, 84 232 wpisów, last sync 5 min, genesis 2026-01-12
- Action buttons: Generator DPIA/FRIA / Raport zgodności (PDF)
- Tabs-bar z aktywną "Audyt"
- Sekcja retencja per klasa (4 karty: A 30 dni / B 14 dni / C 7 dni / Audit log 5 lat)
- Sekcja hash-chain log: filtry, search box, tabela (Czas | Operator | Akcja chip | Opis | Hash). Wpisy przykładowe: alarm.confirm, model.inference, legal_grant.approve, search.attributes, camera.add, retention.purge, unmask.enable, worm.sync
- Sekcja generator dokumentów (3 karty): DPIA, FRIA, Klauzula informacyjna

**Pod spodem:**
- **Hash-chain audit (core):** istniejący `audit_log` table + nowy mechanizm `audit_chain` z Merkle hash chain (każdy wpis ma hash poprzedniego). API: `audit_query(filter)`, `audit_export(time_range, format)`, `audit_verify(from_hash)`. Eksternalizacja do WORM (S3-immutable lub disk z `chattr +a`) co N min
- **Retencja:** background job (core) co 1h: dla każdej klasy danych usuwa zapisy starsze niż retention. Audit wpisuje fakt usunięcia
- **Generator DPIA:** klik "Generuj" → addon zbiera dane (kategorie z manifestu, detektory aktywne, kamery, retencja, modele z wersjami) + user wypełnia (cel, podstawa prawna, ryzyko, mitigacje) → output PDF + zapis `dpia.assessment` table
- **Generator FRIA:** analogicznie ale dla AI Act art. 27 (high-risk)
- **Klauzula informacyjna:** generator PDF (PL/EN/UA) — szablon w `assets/legal/`, addon wypełnia adres deployment, listę detektorów, podstawę prawną
- **Permissions:** `audit.read`, `sql.read/write` (dla DPIA records)

**Klasa danych:** mix — audit zawiera wpisy klasy A/B/C; retention rules respektują klasę.

---

### M11 — Eksport dowodowy

**Cel:** paczki dowodowe (signed HSM + TSA) dla służb uprawnionych. Lista uprawnionych odbiorców, log eksportów z sygnaturą sprawy, łańcuch zaufania (HSM/TSA/anchor).

**Persona:** admin, supervisor uprawniony, LEA-officer.

**Storyboard:**
- Detail-header: HSM Yubikey HSM2 podłączony (chip success), 12 paczek wygenerowanych 30 dni
- Action buttons: Eksport log / Nowa paczka
- Tabs-bar z aktywną "Eksport"
- Sekcja "Uprawnieni odbiorcy" (tabela: organ / klucz publiczny PGP / aktywny)
- Sekcja "Łańcuch zaufania" (tabela: HSM device, klucz podpisu, TSA, audit anchoring, verifier CLI)
- Lista paczek (evidence-card layout, 80px ikona + meta + akcje Pobierz/Weryfikuj):
  - EV-2026-051: 3 klipy 4:32 min, 287 MB, kamery C-04+C-07, sygn. PR-3-K-247/2026, podpisana, TSA OK
  - EV-2026-050: ...

**Pod spodem:**
- **Lista paczek:** `sql_query("SELECT * FROM evidence_exports ORDER BY created_at DESC")` (lokalna tabela addona z metadanymi). Faktyczne pliki w core evidence storage
- **Nowa paczka:** wizard wybór alarm-id / search-results, sygnatura sprawy, organ → `evidence_sign({clip_refs, snapshots, manifest_json: {legal_grant_id, case_no, authority, scope, requested_by}})` → core składa ZIP, dołącza klucze publiczne, podpisuje, dodaje TSA timestamp → `SignedPackage{id, bundle_url, signature, timestamp_token, chain_hash}`
- **Anchoring:** co 24h core grupuje hash-chain heads, robi anchor do BTC mainnet (OpenTimestamps) — sygnatura `chain_hash` w pakietach jest weryfikowalna publicznie
- **Verifier:** addon udostępnia link do CLI `tentavision verify package.tvevidence` — strona otrzymująca może niezależnie sprawdzić
- **Uprawnieni odbiorcy:** tabela `evidence_recipients(authority, pgp_key, active)` — admin TentaFlow zarządza. Paczka dla danej organu jest dodatkowo zaszyfrowana ich kluczem publicznym
- **Audit:** każdy eksport = wpis hash-chain klasy C + obowiązkowo legal_grant_id w manifest
- **Permissions:** `evidence.sign` (gate: deployment_profile_lea_or_critical), `audit.write_classC`

**Klasa danych:** C (zawsze, bo nawet bez D4 to wynos dowodów na zewnątrz).

---

### M12 — Ustawienia addona

**Cel:** storage limits / retencja / backend SQL, inference backends, powiadomienia, licencje, profil prawny (zmiana w trakcie).

**Persona:** admin TentaVision.

**Storyboard:**
- Detail-header: deployment, profil prawny aktywny, wersja v0.4.2
- Action buttons: Anuluj / Zapisz zmiany
- Tabs-bar z aktywną "Ustawienia"
- 4 karty grid 2×2:
  - **Storage & retencja:** lokalizacja nagrań, limit dyskowy, lokalizacja indeksu wektorów, WORM bucket, retencja A/B/C (override z chip "wymaga uzasadnienia")
  - **Inference runtime:** backend (TensorRT/OpenVINO/ONNX), GPU scheduler, max równoczesnych modeli, backpressure policy, warmup toggle, hot reload toggle
  - **Powiadomienia & integracje:** webhook URL, SMS (Twilio), email, Slack channel, web push, wyciszanie nocne
  - **Licencje & klucze:** Pro license, HSM device, TSA URL, anchoring blockchain, camera vault rotacja
- Sekcja "Profil prawny & AI Act" (orange warning bg): dropdown profil deployment + tooltip "zmiana wymaga podpisu DPO + audit"

**Pod spodem:**
- **Storage settings:** KV `storage_set("config:retention:A", "30")` itd. Core czyta przy purge job
- **Inference backend:** wybór wpływa na to które services są preferowane w aliasach (matchowane po `backend` capability w service metadata). Sam runtime nie żyje w addonie
- **Webhook/SMS/Email:** dla każdej notyfikacji: `[[network_rule]]` w manifeście musi obejmować target host. Secret (Twilio token, email pass) przez `secret_set`
- **HSM/TSA:** to konfiguracja core (TentaFlow ma jeden HSM dla wszystkich addonów). Addon czyta przez `evidence_config_get()` (readonly)
- **Zmiana profilu prawnego:** klik "Zapisz" jeśli profil_prawny changed → modal "wymagany podpis DPO" → DPO podpisuje (PIN + secret) → `claim_add({type: "deployment_profile", oneof: ...})` + audit log
- **Permissions:** `storage.write`, `secret.write`, `claim.write`

**Klasa danych:** A (configuration).

---

### M13 — Onboarding wizard (profil prawny)

**Cel:** wybór profilu prawnego (Komercja prywatna / Transport publiczny / Lotnisko-operator / Służby uprawnione) determinuje dostępność D4 i polityk domyślnych. Ten ekran może być częścią M15 install wizard albo osobnym pierwszym uruchomieniem.

**Persona:** DPO + admin przy pierwszym uruchomieniu lub zmianie profilu.

**Storyboard:**
- Welcome screen z gradientowym logo TentaVision + opis 5 min
- Progress bar 4 kroki (1 ukończone "Rola wdrożenia" ✓, aktywny "Profil prawny", reszta pending)
- 4 karty profilu w grid 2×2 (active=Komercja):
  - **Komercja prywatna** (default): D1/D2anon/D3/D5attr/D6 ✓, D4 LOCKED
  - **Transport publiczny (operator):** D1-D3/D5/D6 ✓, D4 historyczne z DPIA, D4 real-time LOCKED
  - **Lotnisko / dworzec (operator):** wszystkie D1-D6, D4 z LegalGrant + FRIA
  - **Służby uprawnione:** wszystkie D1-D6 + D4 real-time pod LegalGrant + wymaga manifestu instalacji
- Info box: co się stanie po wyborze (DPIA template, retention defaults, hash-chain ON, post-market monitoring)
- Buttons: Wstecz / Dalej

**Pod spodem:**
- **Zapis profilu:** `claim_add({type: "deployment_profile", value: "commercial"|"transport"|"airport"|"lea"})` w core policy engine. Każdy profil = template wartości domyślnych dla retencji, gates aktywne, generator szablonów dokumentów
- **Wpływ na D4:** gate `d4-realtime` wymaga `deployment_profile.value IN ["lea", "critical_infra"]`. Bez tego claim nie da się uzyskać aktywnego LegalGrant
- **Audit:** zmiana profilu → audit chain z hash + DPO signature
- **Permissions:** `claim.write` (przez DPO)

**Klasa danych:** A (configuration), ale wpływa na dostęp do C.

---

### M14 — Bindings & Storage (readonly view + statystyki)

**Cel:** dla admin TentaVision: readonly podgląd 6 aliasów AI utworzonych przez addon + statystyki wbudowanych API (KV, SQL, Vector, Recording, Camera, Streaming, Evidence). Link out do globalnego UI aliasów (M16) dla edycji target/fallback.

**Persona:** admin TentaVision, supervisor.

**Storyboard:**
- Detail-header: 5/6 aliasów aktywnych, 1 niezmapowany (warning chip)
- Action buttons: Re-check / Test wszystkich aliasów
- Tabs-bar z aktywną "Bindings"
- Info banner: wyjaśnienie że to readonly + link do M16
- Sekcja "Aliasy AI utworzone przez addon" — tabela 6 wierszy: alias name + methods | current target | strategy | last used target (po fallback) | status (active/shadow/gated/unconfigured). Każdy wiersz link "Skonfiguruj w Serwisy → Aliasy"
- Sekcja "Storage" — 4 karty: KV (147/10k kluczy), SQL backend SQLite 84 MB, Vector store 2.1M wektorów 4 namespaces, Recording 2.4/4 TB
- Sekcja "SQL · zawartość bazy TentaVision" — tabela tabel z rozmiarami, klasą, indeksami. Buttons: Backup snapshot / VACUUM / Migrate → PostgreSQL
- Sekcja "Vector namespaces" — tabela: namespace, wymiary, count, rozmiar, klasa (faces/persons gated)
- 3 karty grid: Camera API stats / Streaming API stats / Evidence API stats

**Pod spodem:**
- **Aliasy readonly:** `alias_list_owned()` zwraca `Vec<AliasInfo{id, methods, current_target, fallback_targets, strategy, last_used_target, last_used_at, calls_24h, is_active}>`
- **Last used target:** core router przy każdym `service_call` zapisuje który target wykonał (z fallback metadata). Statystyki w tabeli `alias_calls(alias_id, target_used, ts, duration_ms, fallback_used)` w core
- **KV stats:** `storage_stats()` (rozszerzenie API) → `{key_count, total_bytes, last_modified}`
- **SQL stats:** specjalne zapytania na własnej bazie addona: `sql_query("SELECT name, (SELECT COUNT(*) FROM pragma_table_info(name)) FROM sqlite_master WHERE type='table'")` + per-table COUNT
- **Vector stats:** dla każdego namespace `vector_count(namespace)` + `vector_storage_size(namespace)`
- **Recording stats:** `recording_stats(camera_id)` → `{disk_used, oldest_segment, segments_count}` agregowane
- **Permissions:** `service.read`, `storage.read`, `sql.read`, `vector.read`, `recording.read`, `camera.read`

**Klasa danych:** A (metadata) — same statystyki, nie treść.

---

### M15 — Install wizard (6 kroków instalacji addona)

**Cel:** standardowy wizard instalacji addona w TentaFlow (uniwersalny dla wszystkich addonów). TentaVision pokazuje 6 kroków: Permissions → Storage → Aliasy AI (deklaracja) → Flow templates → Profil prawny → Pierwsza kamera. To NIE jest ekran addona — to TentaFlow generic wizard renderowany na podstawie manifestu.

**Persona:** admin TentaFlow.

**Storyboard:**
- Header z big-ico, nazwa addona + wersja, lista trybów (application, service tick, 5 tools, 4 flow blocks), warning chip "wymaga HSM, GPU"
- Progress bar 6 kroków (kroki 1-2 done, 3 active, 4-6 pending)
- **Krok 3 — Deklaracja aliasów AI** (na ekranie aktywny):
  - Lista 6 aliasów z manifestu (alias name, methods, suggested_default)
  - Per alias status: "will be created" (success), "created with empty target" (warning), "created inactive (gated)" (lock)
  - **Bez** wyboru konkretnego targetu — to zostaje na M16 po instalacji
  - Info box: "TentaFlow wykona create_or_reactivate_model_alias() dla każdego z 6 aliasów. Po instalacji przejdź do Serwisy → Aliasy (M16) i przypisz target_model, fallback_targets, strategy"
- Details (collapsible) z poprzednich kroków:
  - Krok 1 Permissions: 14 chipów success (z risk-level)
  - Krok 2 Storage: tabela wybrana konfiguracja (KV aktywny, SQL backend SQLite, dialect ANSI, migrations 6 plików, vector 4 namespaces, encryption at-rest)
- Section "Kolejne kroki" (kroki 4-6 podsumowanie)
- Buttons: Wstecz: Storage / Dalej: Flow templates

**Pod spodem (TentaFlow core, nie addon):**
- **Krok 1 Permissions:** czyta manifest `[[permission]]` → renderuje checkboxes pogrupowane po risk-level → admin akceptuje → zapisuje do `addon_permissions(addon_id, permission_id, granted)` per user/group
- **Krok 2 Storage:** czyta manifest `[storage]` → jeśli `sql = true` i `sql_backends` ma więcej niż jeden → pyta admina o wybór → jeśli SQLite → tworzy `~/.tentaflow/addons/tentavision/` → jeśli PostgreSQL → pyta o connection string, tworzy database + role → uruchamia migrations z `migrations_dir`
- **Krok 3 Aliasy (na ekranie):** czyta `[[alias]]` z manifestu, dla każdego sprawdza czy istnieje w `model_aliases` (collision?). Po finalizacji wywołuje `create_or_reactivate_model_alias(alias_id, suggested_default, "first_available")` z `is_active = !gate_required`
- **Krok 4 Flow templates:** czyta `[[flow_template]]`, parsuje `flows/*.flow.json`, pokazuje preview każdego (jakie bloki, capabilities). User wybiera które zaimportować. Imported → wpis w `flows` tabeli core
- **Krok 5 Profil prawny:** wywołuje M13 inline (lub osobny krok)
- **Krok 6 Pierwsza kamera:** opcjonalny — odpala M3 wizard kamery (alternatywnie skip)
- Po finalizacji: addon zaktywowany, service tick startuje, addon dostępny w nav

**Klasa danych:** A.

---

### M16 — Serwisy → Aliasy (globalny UI TentaFlow, NIE addon)

**Cel:** systemowe UI w sekcji Services TentaFlow do konfiguracji wszystkich aliasów (z różnych addonów + manualnych). Per alias: target_model, fallback_targets, strategy, is_active. Inline edit z drag-to-reorder fallbacków.

**Persona:** admin TentaFlow (nie addon TentaVision specific).

**Storyboard:**
- Sidebar TentaFlow: aktywny "Services"
- Breadcrumb: General / Services / Aliasy
- Detail-header: ikona network, 18 aktywnych, 3 niezmapowane (warning)
- Action buttons: search box + Nowy alias (manual)
- Tabs-bar: Wszystkie serwisy / Modele / Aliasy (active) / Węzły / Historia
- Filter chips: po owner (addon/manual), aktywne, strategy, z fallbackami, pusty target
- Tabela aliasów 7 kolumn: Alias | Owner pill (addon/manual) | Target + fallback chain | Strategy chip | Last used + call count | Active toggle | Edit button
- **Inline edit (rozwinięty dla tentavision-yolo):** primary target dropdown + strategy radio cards (first_available / round_robin / weighted) + fallback builder (drag-to-reorder lista) + metadata box + Save/Delete/Cancel
- Help section: jak działają aliasy (strategie, owner, pusty target)

**Pod spodem (TentaFlow core, nie addon TentaVision):**
- **Lista aliasów:** `SELECT * FROM model_aliases ORDER BY alias` + dla każdego count z `alias_calls(alias_id)` (statystyki użycia)
- **Owner:** kolumna w `model_aliases` lub osobna tabela `model_alias_owners(alias_id, owner_type, owner_id)`. Wartości: `manual` (admin) lub `addon:<addon_id>`
- **Edit:** dropdown primary target wypełniony z `service_list()` (wszystkie zarejestrowane services pasujące do kind aliasu)
- **Strategy:**
  - `first_available` — router próbuje primary, fallback przy down
  - `round_robin` — load balancing po wszystkich aktywnych targetach
  - `weighted` — wagi per target (np. 80% primary / 20% canary) — extra pole `weights` w JSON
- **Fallback chain:** lista `fallback_targets` w `model_aliases` (TEXT z CSV lub JSON array)
- **Drag-to-reorder:** custom component lub SortableJS
- **Save:** `UPDATE model_aliases SET target_model=?, fallback_targets=?, strategy=?, is_active=? WHERE alias=?` + audit core
- **Delete alias:** jeśli owner=addon → tylko deaktywacja (`is_active=0`), nie można usunąć (dopóki addon zainstalowany). Jeśli owner=manual → DELETE pełny
- **Nowy alias manual:** modal "Nowy alias", podaj nazwę + target + fallbacks → INSERT
- **Permissions:** wymaga roli admin (nie user-poziom)

**Klasa danych:** A (configuration aliasów). Audit tabela `model_alias_changes` dla compliance.

---

## 5. Manifest TentaVision (finalny)

```toml
# manifest.toml — kompletny manifest TentaVision

[addon]
id = "tentavision"
name = "TentaVision"
version = "0.1.0"
description = "Analiza obrazu z kamer: ADR, anomalie zachowań, bagaż, atrybuty, re-id pod gate prawnym"
category = "surveillance"
keywords = ["video","cctv","analysis","adr","baggage","behavior","reid"]
author = "TentaFlow"
license = "Commercial"
icon = "video"
runtime = "wasmtime"
platforms = ["linux"]
wasm_file = "tentavision.wasm"

# === Tryb 1: aplikacja ============================================
[application]
entry_panel = "dashboard"
title = "TentaVision"
icon = "video"
sort_order = 100

# === Tryb 2: background tick ======================================
[service]
enabled = true
tick_interval_ms = 250          # szybko, bo drenuje streamy
tick_fuel_budget = 5000000
tick_timeout_ms = 1000

[visibility]
admin_only = false
show_in_catalog = true

[resources]
memory_mb = 256
fuel_limit = 10000000
storage_total_mb = 64
http_requests_per_minute = 60

# === Storage ======================================================
[storage]
kv = true
sql = true
sql_backends = ["sqlite", "postgres"]
sql_dialect = "ansi"
migrations_dir = "migrations"
encryption = "at-rest"

# === Vector namespaces ============================================
[[vector_namespace]]
name = "attributes"
dimensions = 768
distance = "cosine"
data_class = "B"

[[vector_namespace]]
name = "plates"
dimensions = 256
distance = "cosine"
data_class = "B"

[[vector_namespace]]
name = "faces"
dimensions = 512
distance = "cosine"
data_class = "C"
gate = "d4-historical"

[[vector_namespace]]
name = "persons"
dimensions = 512
distance = "cosine"
data_class = "C"
gate = "d4-historical"

# === Aliasy AI (deklaracja) — addon utworzy w model_aliases =======
[[alias]]
id = "tentavision-yolo"
display_name = "Detektor obiektów (D1, D6)"
methods = ["detect", "track"]
suggested_default = "yolo11m-detector"

[[alias]]
id = "tentavision-ocr"
display_name = "OCR ADR / tablic rejestracyjnych"
methods = ["recognize", "recognize_cropped"]
suggested_default = "ppocrv5-ocr"

[[alias]]
id = "tentavision-action"
display_name = "Klasyfikator akcji (D2)"
methods = ["classify_window"]
suggested_default = ""              # auto_bind po deployu silnika

[[alias]]
id = "tentavision-vlm"
display_name = "VLM atrybuty (D5)"
methods = ["embed", "caption"]
suggested_default = "siglip2-vit-l14"

[[alias]]
id = "tentavision-face-embed"
display_name = "Face embedding (D4)"
methods = ["embed"]
suggested_default = ""
gate = "d4-historical"

[[alias]]
id = "tentavision-reid"
display_name = "Person re-id (D4)"
methods = ["embed", "match"]
suggested_default = ""
gate = "d4-historical"

# === Permissions (14 sztuk) =======================================
[[permission]]
id = "service.call"
display_name = "Wywołaj zarejestrowane services przez aliasy"
risk = "medium"

[[permission]]
id = "camera.manage"
display_name = "Dodaj/usuń/konfiguruj kamery"
risk = "medium"

[[permission]]
id = "camera.read"
display_name = "Czytaj listę i metadane kamer"
risk = "low"

[[permission]]
id = "stream.subscribe"
display_name = "Subskrybuj strumienie z kamer"
risk = "medium"

[[permission]]
id = "sql.read"
risk = "low"

[[permission]]
id = "sql.write"
risk = "low"

[[permission]]
id = "vector.read"
risk = "low"

[[permission]]
id = "vector.write"
risk = "low"

[[permission]]
id = "recording.save"
display_name = "Zapisuj klipy z ring-buffera"
risk = "medium"

[[permission]]
id = "recording.read"
risk = "medium"

[[permission]]
id = "evidence.sign"
display_name = "Podpisz paczki dowodowe (HSM)"
risk = "high"
gate = "deployment_profile_lea_or_critical"

[[permission]]
id = "event.publish"
risk = "low"

[[permission]]
id = "flow.invoke"
risk = "medium"

[[permission]]
id = "secret.read"
risk = "high"

[[permission]]
id = "secret.write"
risk = "medium"

[[permission]]
id = "ui.render"
risk = "low"

# === Network rules (minimalne, większość przez service_call) =====
[[network_rule]]
id = "webhook-callback"
protocol = "tcp"
host = "*.tentaflow.local"
port = 443
description = "Webhook callback do flow-engine TentaFlow"
required = false

# === Flow templates (opt-in install) ==============================
[[flow_template]]
id = "tv-realtime-adr"
display_name = "Real-time analiza ADR"
path = "flows/tv-realtime-adr.flow.json"
description = "Pipeline: frame → yolo (vehicle) → yolo (plate) → ocr → legibility scorer → ADR validator → event"

[[flow_template]]
id = "tv-alarm-enrich"
display_name = "Wzbogacenie alarmu"
path = "flows/tv-alarm-enrich.flow.json"
description = "Po alarmie: save clip → snapshots → vector embed → store metadata → notify"

[[flow_template]]
id = "tv-evidence-export"
display_name = "Eksport dowodowy"
path = "flows/tv-evidence-export.flow.json"
description = "Zbiera klipy + snapshoty + grant → evidence_sign → bundle"

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
[[tool.parameter]]
name = "camera_filter"
param_type = "string"
required = false

[[tool]]
id = "check_adr"
description = "Sprawdź czytelność tablicy ADR na ostatnim kadrze kamery"
[[tool.parameter]]
name = "camera_id"
param_type = "string"
required = true

[[tool]]
id = "confirm_alarm"
description = "Potwierdź, odrzuć lub eskaluj alarm"
[[tool.parameter]]
name = "alarm_id"
param_type = "string"
required = true
[[tool.parameter]]
name = "verdict"
param_type = "string"
required = true

[[tool]]
id = "run_flow"
description = "Uruchom skonfigurowany Flow z TentaVision"
[[tool.parameter]]
name = "flow_id"
param_type = "string"
required = true
[[tool.parameter]]
name = "input"
param_type = "object"
required = true

[[tool]]
id = "export_evidence"
description = "Wygeneruj podpisaną paczkę dowodową dla wskazanego alarmu / wyników search"
[[tool.parameter]]
name = "alarm_ids"
param_type = "array"
required = false
[[tool.parameter]]
name = "case_no"
param_type = "string"
required = true

# === Gates (claims-based, generic) ===============================
[[gate]]
id = "d4-historical"
display_name = "Re-identyfikacja historyczna (D4)"
required_claims = [
  { type = "approval", subject = "dpia", status = "signed" },
  { type = "grant", scope = "biometric:historical", valid = true, has_expiry = true },
]

[[gate]]
id = "d4-realtime"
display_name = "Re-identyfikacja w czasie rzeczywistym"
required_claims = [
  { type = "approval", subject = "dpia", status = "signed" },
  { type = "approval", subject = "fria", status = "signed" },
  { type = "grant", scope = "biometric:realtime", valid = true, has_expiry = true },
  { type = "deployment_profile", oneof = ["lea", "critical_infra"] },
]

[[gate]]
id = "deployment_profile_lea_or_critical"
required_claims = [
  { type = "deployment_profile", oneof = ["lea", "critical_infra"] },
]

# === Custom UI components (sandboxed) =============================
[[ui_component]]
id = "tv-video-grid"
display_name = "Grid kamer z bbox overlay"
slot = "main"
src = "components/tv-video-grid.js"
signature = "ed25519:base64=..."
risk = "high"                   # iframe sandbox

[[ui_component]]
id = "tv-zone-editor"
display_name = "Edytor stref polygonowych"
slot = "main"
src = "components/tv-zone-editor.js"
signature = "ed25519:base64=..."
risk = "high"

[[ui_component]]
id = "tv-heatmap"
display_name = "Heatmapa aktywności 24h"
slot = "main"
src = "components/tv-heatmap.js"
signature = "ed25519:base64=..."
risk = "low"                    # shadow DOM, prosty render

# === GPU info-only ================================================
[gpu]
recommended_vram_mb = 12000
notes = "Dla pełnego profilu D2+D5 zalecane 24 GB; D4 wymaga osobnego node-a"

# === Konfiguracja eksponowana adminowi =============================
[config.schema]
default_flow_realtime = { type = "string", default = "tv-realtime-adr" }
default_flow_alarm = { type = "string", default = "tv-alarm-enrich" }
default_flow_export = { type = "string", default = "tv-evidence-export" }
deployment_profile = { type = "string", default = "commercial" }
worm_bucket = { type = "string", default = "" }
tsa_url = { type = "string", default = "https://freetsa.org/tsr" }
maskowanie_default = { type = "bool", default = true }
night_mode_hours = { type = "string", default = "22:00-06:00" }
```

---

## 6. SDK API gaps — co musi być dopisane do TentaFlow

Konsolidacja 15 API (po §21 korekcie). Każda pozycja: opis, dlaczego potrzebna, kto wywołuje, audit, permission.

### API-1: `service_call(alias, method, payload) → ServiceResponse`

**Co:** Rozszerzenie istniejącego `service_request` o `method` (dziś payload pakowany w `CompletionPayload`). Nowa response z metadanymi.

**Response:**
- `payload` (bajty)
- `executed_by` — który konkretny target wykonał (po rozwiązaniu aliasu + fallbackach)
- `duration_ms`
- `fallback_used` — czy router musiał spaść na fallback

**Dlaczego:** addon nie wie a priori kto rozwiąże alias; widoczność w runtime jest niezbędna do audytu + obserwacji fallbacków.

**Audit:** każde wywołanie z `(addon_id, alias, method, executed_by, duration, ok/err)`.

**Permission:** `service.call`.

**Priorytet:** F1 BLOCKER.

### API-1a: `alias_create / alias_deactivate / alias_get / alias_list_owned`

**Co:** Host functions do zarządzania aliasami z poziomu addona. Dziś teams-bot wywołuje wewnętrznie `repository::create_or_reactivate_model_alias` przez Rust — nie ma ABI dla WASM addonów.

**Funkcje:**
- `alias_create(spec: AliasSpec)` — addon tworzy alias w `model_aliases` z `suggested_default` jako `target_model` i `is_active = !gate_required`. Owner=this addon
- `alias_deactivate(alias_id)` — ustaw `is_active=0` (np. przy disable addona). NIE usuwa
- `alias_get(alias_id) → AliasInfo` — readonly: aktualny target, fallback chain, strategy, ostatnio użyty target, statystyki użycia
- `alias_list_owned() → Vec<AliasInfo>` — tylko aliasy utworzone przez ten addon

**Audit:** każda zmiana z `(addon_id, alias, action, value_before, value_after)`.

**Permission:** `alias.manage` (nowa).

**Priorytet:** F1 BLOCKER.

### API-1b: `[[alias]]` w manifeście

**Co:** Sekcja manifestu deklarująca aliasy które addon utworzy przy aktywacji.

**Pola:** `id`, `display_name`, `methods` (lista), `suggested_default` (może być pusty), `gate` (opcjonalnie nazwa gate'a).

**Dlaczego:** dziś teams-bot ma hard-coded `TEAMS_BOT_ALIASES` w mod.rs — trzeba przenieść do manifestu żeby addony zewnętrzne mogły deklarować.

**Priorytet:** F1 BLOCKER.

### API-1c: UI Services → Aliasy w globalnym TentaFlow www

**Co:** Mockup M16 — ekran w `/services/aliases` w global UI TentaFlow (nie w addonie).

**Funkcjonalność:** lista wszystkich aliasów z `model_aliases`, edycja `target_model + fallback_targets + strategy`, drag-to-reorder fallbacków, owner pill (addon/manual), statystyki użycia, manual alias create.

**Dlaczego:** dziś brak UI dla aliasów — tylko CLI/SQL. Bez tego admin nie ma jak skonfigurować aliasów które addon utworzył.

**Priorytet:** F1 BLOCKER.

### API-2: `[storage]` w manifeście + per-addon storage paths

**Co:** Sekcja manifestu deklarująca jaki storage addon używa.

**Pola:**
- `kv` (bool, default true) — czy używa istniejącego storage_get/set
- `sql` (bool, default false) — czy używa nowego SQL API
- `sql_backends` (lista: `["sqlite"]` / `["postgres"]` / `["sqlite","postgres"]`) — wymagane gdy `sql=true`
- `sql_dialect` (`ansi` / `sqlite` / `postgres`) — co dialekt addon używa
- `migrations_dir` (string) — katalog z migration files
- `encryption` (`none` / `at-rest`) — SQLCipher dla SQLite, pgcrypto dla PG

**Infrastructure (core):**
- Per-addon FS sandbox: `~/.tentaflow/addons/<addon_id>/data.db` dla SQLite
- Per-addon PG database + role dla PostgreSQL (jeśli wybrany)
- Migrations runner uruchamiany przy install + każdym version bump

**Priorytet:** F1 BLOCKER (przy SQL API).

### API-3: SQL host functions

**Co:**
- `sql_exec(query, params) → rows_affected`
- `sql_query(query, params) → Vec<Row>`
- `sql_query_one(query, params) → Option<Row>`
- `sql_transaction(statements) → ()` — atomic batch

**Backend:** core proxy do per-addon SQLite lub do per-addon PG database (wybór przy instalacji w M15). Addon nie wie pod spodem.

**Bezpieczeństwo:** parametryzacja obowiązkowa (bind params, nie string concat). Core nie pozwala na DDL z runtime (CREATE/ALTER tylko przez migrations).

**Audit:** każde write (`sql_exec`) z hash query (nie pełny query żeby nie wyciekły dane). Read sample audit (co N-te).

**Permissions:** `sql.read`, `sql.write`.

**Priorytet:** F1 BLOCKER.

### API-5: Camera API (host functions)

**Funkcje:**
- `camera_add(spec) → CameraId`
- `camera_list(filter) → Vec<CameraInfo>`
- `camera_get(id) → CameraInfo`
- `camera_update(id, patch) → ()`
- `camera_remove(id) → ()`
- `camera_snapshot(id) → ImageRef` — jednorazowy obraz
- `camera_discover(network_hint, timeout) → Vec<DiscoveredCamera>` — ONVIF + mDNS + ARP
- `camera_test_connection(spec) → CameraCapabilities`
- `camera_credentials_rotate(id) → ()`
- `camera_health(id) → CameraHealth`

**Spec ważne:** `credentials_secret_ref` (nie plaintext) — addon zapisuje secret przez `secret_set`, dostaje ref.

**Backend (core):** moduł `tentaflow-camera-ingest` — RTSP/ONVIF/Hikvision ISAPI/Dahua CGI/Axis VAPIX/UniFi Protect connectors. Trzyma sesje per kamera, retry, jitter, vendor quirks. Wystawia QUIC API do innych services (recording, vision services pobierają frames po ref).

**Audit:** `camera_add/update/remove` z user_id + camera_id + zmiany. Snapshot/health = sample audit.

**Permissions:** `camera.manage`, `camera.read`.

**Priorytet:** F1 BLOCKER.

### API-6: Streaming API + FrameRef opaque

**Funkcje (pull-based):**
- `stream_subscribe(target, filter) → StreamId`
- `stream_next(id, timeout_ms) → Option<StreamMessage>`
- `stream_close(id) → ()`

**Targets:**
- `Camera{id, sample_fps}` — raw frame refs
- `DetectorEvents{profile_id}` — eventy z aktywnych Flow detection
- `EventBus{topic_pattern}` — generic events (np. `alarm.*`)

**Messages:**
- `Frame{camera_id, ts, frame_ref: FrameRef, sequence}`
- `Event{camera_id, ts, kind, payload}`
- `Detection{camera_id, ts, frame_ref, boxes}`
- `Drop{count, reason}` — backpressure signal
- `End{reason}`

**FrameRef:** opaque uchwyt — addon nie ma bajtów. Może przekazać `frame_ref` do `service_call`, gdzie konkretny service pobierze ramkę z core po referencji (Service-to-Core API).

**Backpressure:** core dropuje najstarsze wiadomości gdy addon nie drenuje w czasie. Zwraca `Drop{count}` jako informację.

**Permission:** `stream.subscribe`.

**Priorytet:** F1 BLOCKER.

### API-7: Vector store API

**Funkcje:**
- `vector_upsert(namespace, items) → ()`
- `vector_search(namespace, query, k, filter) → Vec<Hit>`
- `vector_delete(namespace, ids) → ()`
- `vector_count(namespace) → u64`
- `vector_storage_size(namespace) → u64`

**Backend:** embedded HNSW (np. `hnsw_rs` crate) z persystencją do `~/.tentaflow/vector/<addon_id>/<namespace>.hnsw`. Per-addon sandbox.

**Manifest:** `[[vector_namespace]]` deklaruje namespace z dimensions, distance, data_class, optional gate.

**Filter:** metadata predicates (np. `camera_id IN [...] AND ts BETWEEN [...]`).

**Audit:** upsert/delete z classified count. Search bez payload (tylko metadata: namespace, k, hits_count).

**Permissions:** `vector.read`, `vector.write`.

**Priorytet:** F2.

### API-8: Recording API

**Funkcje:**
- `recording_save_segment(camera_id, start_ts, end_ts) → ClipRef`
- `recording_save_snapshot(camera_id, ts) → SnapshotRef`
- `recording_get_stream(ref) → StreamHandle` — live download
- `recording_get_url(ref, ttl_sec) → Url` — signed URL dla frontendu (`<video src>`)
- `recording_purge(ref) → ()` — honoruje retention policy
- `recording_stats(camera_id) → RecordingStats`

**Backend (core):** moduł `tentaflow-recording-manager` z ring-bufferem per kamera. Segmenty MP4 (np. 5-min) trzymane do retention limit. Każdy event w core może triggerować "save segment" który zatrzymuje konkretny przedział od purge.

**ClipRef / SnapshotRef:** opaque uchwyty. Addon nigdy nie zna ścieżki.

**Permissions:** `recording.save`, `recording.read`.

**Priorytet:** F2.

### API-9: Evidence API

**Funkcje:**
- `evidence_sign(payload) → SignedPackage`
- `evidence_verify(package) → VerifyResult`
- `evidence_anchor(package_id) → AnchorRef`

**Payload:**
- `clip_refs: Vec<ClipRef>`
- `snapshots: Vec<SnapshotRef>`
- `manifest_json: String` — legal_grant_id, case_no, authority, scope, requested_by

**SignedPackage:**
- `id`, `bundle_url` (signed URL dla download)
- `signature` (HSM private key, np. Ed25519)
- `timestamp_token` (TSA RFC 3161)
- `chain_hash` (link do audit chain)

**Backend (core):** HSM device (Yubikey HSM2 / SoftHSM dev), TSA client (RFC 3161 query), opcjonalnie OpenTimestamps anchor do BTC.

**Audit:** każdy sign z risk_class=C, manifest_json hash w audit, link do legal_grant.

**Permissions:** `evidence.sign` (gate: `deployment_profile_lea_or_critical`).

**Priorytet:** F2.

### API-10: Custom UI components z sygnaturą + iframe sandbox

**Manifest:** `[[ui_component]]` z `id`, `slot`, `src` (ścieżka do JS), `signature` (Ed25519 podpis bundle), `risk` (low/medium/high).

**Renderowanie:**
- `risk = low` → shadow DOM w stronie głównej (CSP `script-src 'self'`, brak inline/eval, allowlist API przez postMessage bridge)
- `risk = high` → iframe sandbox `sandbox="allow-scripts"` (bez `allow-same-origin`) z postMessage bridge

**Bridge API:** enumerated operations: `get_panel_state`, `set_value`, `emit_event`, `request_camera_snapshot(id)`, `request_recording_url(clip_ref)`. Wszystkie audytowane.

**Signature check:** core przy install weryfikuje podpis Ed25519 wszystkich `[[ui_component]]`. Bez signatury / niewłaściwa = odrzucenie instalacji.

**Priorytet:** F2 (M2 video grid, M9 zone editor, M1 heatmap potrzebują tego).

### API-11: Flow invoke

**Funkcje:**
- `flow_invoke(flow_id, input) → RunId`
- `flow_status(run_id) → FlowStatus` — pending/running/success/error + last output
- `flow_list(filter) → Vec<FlowInfo>` — np. po `requires_capabilities`

**Backend:** core FlowBuilder runner, istniejący ale bez WASM ABI.

**Audit:** każdy invoke z `(addon_id, flow_id, run_id, input_hash)`.

**Permission:** `flow.invoke`.

**Priorytet:** F2.

### API-12: `[[flow_template]]` opt-in install

**Manifest:** lista szablonów Flow w bundle addona. Pola: `id`, `display_name`, `path` (do flow.json), `description`.

**Wizard install (M15 krok 4):** pokazuje preview każdego template (bloki, capabilities). User świadomie wybiera które importować. Imported → `flows` tabela core jako addon-owned.

**Priorytet:** F3.

### API-13: Audit `risk_class` enum

**Co:** rozszerzenie audit_log o pole `risk_class: A | B | C | unclassified`.

**Użycie:** host functions które dotyczą danych klasy C (face/reid query, unmask, evidence sign) ustawiają `risk_class=C`. Default `unclassified` lub klasa danych z manifestu.

**WORM:** wpisy klasy C lecą do osobnego WORM bucket (twardsza retencja, dłuższy archive).

**Priorytet:** F3.

### API-14: `on_install(ctx)` z multi-step wizard

**Co:** Lifecycle hook dla complex install. Addon może zwrócić multi-step wizard tree (zamiast generic wizard z manifestu).

**Use case TentaVision:** nie używamy — wszystko mieści się w generic wizardzie M15. Ale przyszłe addony mogą potrzebować.

**Priorytet:** F3.

### API-15: Generic policy / claims engine

**Co:**
- `policy_claims` tabela: `claim(id, type, subject, status, scope, expiry, signed_by, created_at, audit_chain_hash)`
- `claim_add(spec) → ClaimId`
- `claim_revoke(id, reason) → ()`
- `gate_check(gate_id) → GateStatus{required, satisfied: bool, missing_claims: []}`
- Host function `gate_enforce(gate_id) → Result<()>` — wywoływana przed operacjami klasy C

**Manifest:** `[[gate]]` deklaruje required_claims (z typami: approval, grant, deployment_profile).

**Use case:** D4 face_embed alias jest `is_active=0` dopóki `gate_check("d4-historical")` nie zwraca satisfied. Każdy `service_call("tentavision-face-embed", ...)` jest poprzedzony `gate_enforce` po stronie core.

**Priorytet:** F3.

---

## 7. Modele AI i ich rola

Dla każdej domeny: rekomendacja produkcyjna (2026 SOTA + uwzględnione codex review), alternatywa do benchmarku, CPU fallback.

### D1 — ADR (kontrola tablic chemicznych)

| Krok | Produkcja (2026) | Alternatywa | CPU fallback |
|------|------------------|-------------|---------------|
| Detekcja cysterny | YOLO11m (custom fine-tune transport drogowy) | RF-DETR, YOLO12 | YOLO11n |
| Detekcja tablicy ADR | YOLO11s (fine-tune ~2k zdjęć) | RT-DETR | YOLO11n |
| OCR cyfr UN + Kemler | PP-OCRv5 (paddleocr 3.x) lub PARSeq fine-tuned | TrOCR | Tesseract (słaba) |
| Klasyfikacja czytelności | ResNet50 binarny + score legibility | EfficientNet-B0 | mniejszy ConvNet |
| Walidacja ADR | tabela referencyjna ADR 2025 (regex + lookup, lokalnie w wasm asset) | — | — |

**Wynik:** event `adr_check { vehicle_box, un_code, kemler, hazard_class, legibility_score, photo_ref }`.

### D2 — Anomalie zachowań

| Poddomena | Produkcja | Uwagi |
|-----------|-----------|-------|
| Pose + tracking (wspólny) | YOLO11-pose + BoT-SORT | dane wspólne dla wszystkich D2 |
| Upadek / zasłabnięcie | heurystyka kątów kości + lightweight temporal CNN (small TimeSformer) | okno 2s aby zbić FP |
| Agresja / bójka | **VideoMAE V2** lub **InternVideo2** fine-tuned (RWF-2000 + site-specific) | precyzja priorytet, FP <5% wymaga lokalnej kalibracji |
| Broń (pistolet, nóż, długa) | YOLO11m fine-tune (WeaponS, Sohas + site-specific) | wysokie FP → human-in-loop confirmation |
| Wandalizm | klasyfikator akcji (VideoMAE V2) + change detection | często post-event |

### D3 — Pozostawiony bagaż

| Krok | Produkcja |
|------|-----------|
| Detekcja bagażu | YOLO11m (COCO + ABODA + Tumult fine-tune) — suitcase/backpack/handbag |
| Tracking | BoT-SORT / StrongSORT z appearance embed |
| Asocjacja bagaż↔osoba | deterministyczne reguły geometryczne + IoU history |
| Re-id osoby (powrót) | TransReID lub CLIP-ReID (OSNet jako CPU fallback) |

Konfigurowalne: próg czasu (default 90s), strefa wykluczeń (ławka, kasy), godziny ciszy.

### D4 — Re-identyfikacja (strefa wysokiego ryzyka)

| Komponent | Produkcja | Embed size |
|-----------|-----------|-----------|
| Face detect | SCRFD-10g | — |
| Face embed | **AdaFace** (lepszy baseline na low-quality CCTV niż ArcFace/MagFace) | 512 |
| Person detect | YOLO11m + BoT-SORT | — |
| Person re-id | **TransReID** lub **CLIP-ReID** (legacy OSNet jako CPU fallback) | 512–768 |
| Gait (eksperymentalne) | GaitBase z dokumentacją ograniczeń | 256 |

### D5 — Wyszukiwanie po atrybutach

| Atrybut | Model |
|---------|-------|
| Open-vocab "czerwona kurtka, czapka" | **SigLIP / SigLIP2** lub **EVA-CLIP** + dedykowane attribute heads (sam VLM halucynuje) |
| Tablice rejestracyjne | LPRNet / DTRB + walidator PL/EU |
| Marka/model/kolor auta | YOLO11 + klasyfikator (VeRi-776 + Stanford Cars fine-tune) |
| Wiek/płeć szacunkowo | **WYŁĄCZONE domyślnie** (RODO/AI Act high-risk). Włączane tylko per-deployment z legal grant |

### D6 — Generic object detection

YOLO11 (n/s/m wg HW). Custom-class support (transfer learning). Dashboard: heatmapy, liczniki, zone-based counts.

---

## 8. Connectory kamer

Każdy connector w core moduł `tentaflow-camera-ingest`. TentaVision addon nie ma kontaktu z RTSP/ONVIF/Protect — tylko przez Camera API.

| Vendor / Protokół | Priorytet | Notatki |
|-------------------|-----------|---------|
| RTSP universal (TCP/UDP, H.264/H.265) + HTTP snapshot fallback | **P0** | must-have |
| ONVIF Profile S (live) | P0 | discovery + RTSP |
| ONVIF Profile T (advanced streaming) | P0 | H.265, eventy |
| ONVIF Profile M (analytics metadata) | **P0** | edge analytics na kamerze (ANPR, line crossing) — niewynalezione na nowo |
| ONVIF Profile G (recording/search) | **P1** | forensics i historyczne query |
| Hikvision ISAPI | P1 | wariancje firmware/region, ONVIF często off; ANPR onboard |
| Dahua CGI/DSS | P1 | analogicznie wariancje |
| Axis VAPIX + ACAP | P1 | edge analytics na kamerze |
| Hanwha (WiseNet) | P2 | enterprise, dobre eventy |
| Bosch | P2 | enterprise, IVA onboard |
| Avigilon / Motorola | P2 | enterprise security |
| Milestone XProtect | P2 | import source (VMS overlay) |
| Genetec | P2 | import source |
| Frigate | P2 | OSS, migracja / co-existence |
| UniFi Protect | **P2** (zmiana z P1) | API niestabilne, pinować wersje, RTSP fallback obowiązkowy |
| Reolink | P3 | konsumencki |
| MJPEG / HTTP push | P3 | legacy |
| File replay (mp4/mkv) | P0 | dev + forensics |

**Auto-discovery:** ONVIF WS-Discovery + mDNS + ARP scan. M3 wizard.

**Gotchas (wykrywane automatycznie przez camera-ingest, raportowane do UI):**
- firmware tier
- region lock
- ONVIF disabled (Hikvision 5.7.x default)
- digest auth quirks
- TLS cipher mismatch
- admin permission requirement

**Recording strategy:** core ma własny ring-buffer (Recording API) — preferowane. Dla VMS-owych vendorów (UniFi Protect, Milestone) opcjonalnie hybrid: VMS trzyma, my czytamy.

---

## 9. Dane

### 9.1 SQL schema TentaVision (ANSI subset)

Schemat trzymany w plikach `migrations/001_init.sql`, `002_*.sql`, ... w bundle addona. Uruchamiane przez core przy install + każdym version bump.

```sql
-- cameras: lokalna replika info kamer (głównie do szybkich query)
CREATE TABLE cameras (
  id              TEXT PRIMARY KEY,
  vendor          TEXT NOT NULL,
  url             TEXT NOT NULL,
  credentials_ref TEXT,                    -- SecretRef
  location        TEXT,
  retention_class TEXT NOT NULL,           -- A/B/C
  ownership       TEXT NOT NULL,           -- 'tentavision' lub inny addon
  shared_with     TEXT,                    -- JSON array z addon_ids
  added_at        INTEGER NOT NULL,
  last_seen       INTEGER,
  health_flags    TEXT                     -- JSON: backpressure, image_dark, clock_drift
);
CREATE INDEX idx_cameras_vendor ON cameras(vendor);

-- profiles: konfiguracja "co analizować na kamerze i kiedy"
CREATE TABLE profiles (
  id           TEXT PRIMARY KEY,
  name         TEXT NOT NULL,
  flow_id      TEXT NOT NULL,              -- FK do flows w core
  schedule     TEXT,                       -- JSON harmonogram tygodniowy
  retention    TEXT,
  data_class   TEXT NOT NULL,              -- A/B/C
  active       INTEGER NOT NULL DEFAULT 1,
  quick_params TEXT                        -- JSON overrides do Flow inputs
);

-- profile_cameras: many-to-many
CREATE TABLE profile_cameras (
  profile_id TEXT NOT NULL,
  camera_id  TEXT NOT NULL,
  PRIMARY KEY (profile_id, camera_id)
);

-- alarms: alarmy z detektorów
CREATE TABLE alarms (
  id          TEXT PRIMARY KEY,
  ts          INTEGER NOT NULL,
  camera_id   TEXT NOT NULL,
  detector    TEXT NOT NULL,               -- D1/D2/D3/D5/D6
  subtype     TEXT,                        -- np. 'weapon' w D2
  confidence  REAL,
  status      TEXT NOT NULL,               -- pending/confirmed/rejected/escalated
  clip_ref    TEXT,
  operator_id TEXT,
  notes       TEXT,
  confirmed_at INTEGER
);
CREATE INDEX idx_alarms_ts ON alarms(ts);
CREATE INDEX idx_alarms_camera_ts ON alarms(camera_id, ts);
CREATE INDEX idx_alarms_status ON alarms(status);

-- recordings_meta: lokalny index ClipRef ↔ alarm + metadane
CREATE TABLE recordings_meta (
  clip_ref   TEXT PRIMARY KEY,
  camera_id  TEXT NOT NULL,
  start_ts   INTEGER NOT NULL,
  end_ts     INTEGER NOT NULL,
  hash       TEXT,
  alarm_id   TEXT,
  created_at INTEGER NOT NULL
);
CREATE INDEX idx_recordings_camera ON recordings_meta(camera_id);

-- legal_grants: aktywne LegalGrants do D4
CREATE TABLE legal_grants (
  id            TEXT PRIMARY KEY,
  authority     TEXT NOT NULL,
  case_no       TEXT NOT NULL,
  expiry        INTEGER NOT NULL,
  scope         TEXT NOT NULL,             -- 'biometric:historical' / 'biometric:realtime'
  dpo_signature TEXT,
  signed_by     TEXT NOT NULL,             -- supervisor uprawniony
  issued_at     INTEGER NOT NULL,
  is_active     INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX idx_grants_expiry ON legal_grants(expiry);

-- zones: strefy polygonowe per kamera
CREATE TABLE zones (
  id           TEXT PRIMARY KEY,
  camera_id    TEXT NOT NULL,
  name         TEXT NOT NULL,
  type         TEXT NOT NULL,              -- polygon/line/exclude
  polygon_json TEXT NOT NULL,              -- lista {x_pct, y_pct}
  color        TEXT,
  used_by_detectors TEXT                   -- JSON array klasy detektorów
);

-- schedules: harmonogram per kamera (godziny aktywne)
CREATE TABLE schedules (
  camera_id  TEXT NOT NULL,
  day_of_week INTEGER NOT NULL,            -- 0-6
  hour_from  INTEGER NOT NULL,             -- 0-23
  hour_to    INTEGER NOT NULL,
  profile_id TEXT NOT NULL,                -- który profil aktywny w tym slocie
  PRIMARY KEY (camera_id, day_of_week, hour_from)
);

-- rules: reguły kompozytowe AND/OR
CREATE TABLE rules (
  id         TEXT PRIMARY KEY,
  name       TEXT NOT NULL,
  expression TEXT NOT NULL,                -- mini-DSL
  action     TEXT NOT NULL,                -- alarm_critical, sms_dispatch, ...
  active     INTEGER NOT NULL DEFAULT 1
);

-- evidence_exports: zapisane paczki dowodowe (metadata)
CREATE TABLE evidence_exports (
  id           TEXT PRIMARY KEY,           -- EV-2026-XXX
  case_no      TEXT NOT NULL,
  authority    TEXT NOT NULL,
  legal_grant_id TEXT,
  signed_package_id TEXT NOT NULL,         -- ref do core evidence storage
  chain_hash   TEXT NOT NULL,
  created_at   INTEGER NOT NULL,
  created_by   TEXT NOT NULL
);

-- search_audit: lokalna replika krytycznych query (D5 atrybuty, D4 face)
CREATE TABLE search_audit (
  id           TEXT PRIMARY KEY,
  user_id      TEXT NOT NULL,
  query_type   TEXT NOT NULL,              -- text/attribute/similarity/plate/face/person
  query_hash   TEXT NOT NULL,              -- nie pełny query, hash dla compliance
  time_window_from INTEGER,
  time_window_to   INTEGER,
  hits_count   INTEGER,
  legal_grant_id TEXT,                     -- dla D4
  risk_class   TEXT,                       -- A/B/C
  ts           INTEGER NOT NULL
);
CREATE INDEX idx_search_audit_ts ON search_audit(ts);
```

### 9.2 Vector namespaces

| Namespace | Wymiary | Distance | Klasa | Gate | Co indeksuje |
|-----------|---------|----------|-------|------|--------------|
| attributes | 768 | cosine | B | — | SigLIP2 embedding kadrów osób (D5 search) |
| plates | 256 | cosine | B | — | Embedding znaków tablic rejestracyjnych (D5 plate) |
| faces | 512 | cosine | C | d4-historical | AdaFace embedding twarzy (D4) |
| persons | 512 | cosine | C | d4-historical | TransReID embedding sylwetek (D4) |

### 9.3 Recording

| Aspekt | Specyfikacja |
|--------|--------------|
| Format | MP4 H.264/H.265, segmenty 5-min |
| Ring-buffer | Per kamera, w core (nie addon). Limit: 4 TB total, retention per kamera z `cameras.retention_class` |
| Save segment | `recording_save_segment(cam, start, end)` zwraca ClipRef. Segment "wykuty" z ring buffera — nie zostanie purged przed retention klasy |
| Snapshot | Pojedyncza klatka PNG, `recording_save_snapshot(cam, ts)` |
| Access | Tylko przez signed URLs `recording_get_url(ref, ttl)`. Addon nie ma file paths |
| Purge | Background job (core) co 1h, respektuje retention per ClipRef |

---

## 10. RODO / EU AI Act / claims-based gates

### 10.1 Klasyfikacja detektorów

| Klasa | Detektory | RODO/AI Act |
|-------|-----------|-------------|
| **A** niskie | D1 (cysterny, bezosobowe), D3 (bagaż jako obiekt), D6 generic | RODO art. 6.1.f (uzasadniony interes) + signage. AI Act poza Annex III |
| **B** średnie | D2 zachowania (sylwetka anonim), D3 z asocjacją osoby, D5 atrybuty (bez biometrii) | DPIA wymagane; signage; krótka retencja |
| **C** wysokie | D4 face/person re-id/gait, D5 wiek/płeć | **AI Act Annex III high-risk**. Real-time w publicznej przestrzeni **Art. 5** — zakazane poza wąskimi wyjątkami (zaginieni, terroryzm, ciężkie przestępstwa z autoryzacją sądową) |

### 10.2 Twarde mechanizmy (egzekwowane na poziomie core, nie tylko UI)

1. **Profil prawny przy onboardingu (M13/M15):** Komercja prywatna / Transport publiczny / Lotnisko-operator / Służby uprawnione. Profil determinuje **dostępność** detektorów klasy C
2. **Gate `d4-realtime` / `d4-historical`:** addon w manifeście deklaruje `[[gate]] required_claims = [...]`. Core ma policy engine który sprawdza claims przed każdym wywołaniem aliasu klasy C. Bez claims → odmowa
3. **Aliasy klasy C tworzone z `is_active=0`:** dopóki claims nie są spełnione, alias istnieje w `model_aliases` ale jest nieaktywny — router odmawia
4. **Retencja per klasa:** A:30 / B:14 / C:7 dni (override tylko z uzasadnieniem prawnym → audit)
5. **Maskowanie twarzy:** core camera-ingest module blur-uje twarze w klatkach które trafiają do live view dla operator I linii. Unmask wymaga osobnego claim
6. **Right to be forgotten:** narzędzie w M10 → `vector_delete("faces", [subject_id])` + lista żądań RODO + termin
7. **Audit hash-chain:** wszystkie query/eksporty klasy B/C, każdy unmask → append-only + WORM
8. **Generator dokumentów:** szablony DPIA, FRIA, klauzul informacyjnych, znaków monitoring+AI

### 10.3 Profil "Służby" nie jest magiczną rolą

Profil `lea` daje **dostępność** D4, ale każde uruchomienie real-time / każdy eksport wymaga aktywnego `LegalGrant` z:
- authority (Policja / Prokuratura / ABW / SG)
- case_no (sygnatura)
- expiry (data wygaśnięcia uprawnienia)
- podpis kierownika jednostki (DPO sign)
- automatyczne powiadomienie DPO

### 10.4 EU AI Act timeline

- 2.02.2025 — prohibitions + AI literacy
- 2.08.2025 — GPAI obligations
- **2.08.2026 — Annex III high-risk obligations** (D4 wchodzi w pełen reżim)
- TentaVision projektowany teraz musi być compliant z dniem 1

### 10.5 Wbudowane referencje (w bundle addona)

W `assets/legal/`:
- EU AI Act 2024/1689 (art. 5, art. 11, Annex III, Annex IV)
- EDPB Guidelines 3/2019 (video processing)
- RODO art. 6, 9, 35
- Ustawa o ochronie osób i mienia (PL)
- Ustawa o Policji (art. 20)
- KPK art. 217 (zabezpieczenie dowodów)
- ADR 2025 (tabela)
- Szablony DPIA, FRIA, klauzul

---

## 11. Bezpieczeństwo

| Aspekt | Mechanizm |
|--------|-----------|
| Komunikacja z kamerami | TLS gdzie możliwe, RTSPS preferowane (camera-ingest module) |
| Poświadczenia kamer | Secret vault TentaFlow + scheduler rotacji (90 dni default) |
| SSRF hardening | Camera connector w core sprawdza allowlist sieci, blok metadata endpoints. Addon w ogóle nie ma dostępu do RTSP |
| Segmentacja sieci | Kamery w dedykowanym VLAN, runtime w innym (admin TentaFlow konfiguruje) |
| Audit tamper-resistant | Append-only + Merkle chain + WORM externalizacja (S3 immutable lub disk z `chattr +a`) |
| mTLS node-to-node | Już istnieje w TentaFlow |
| Role | viewer / operator / analyst / dpo / admin / lea-officer. Permissions matrix per host function |
| HSM signing | Yubikey HSM2 (prod) / SoftHSM (dev) dla evidence_sign |
| TSA | RFC 3161 (freetsa.org default, alternatywa digistamp) |
| Anchoring | Opcjonalnie BTC mainnet (OpenTimestamps) — daily anchor chain heads |
| Anti-tamper indeksu twarzy | Hash bazy w audit, alarm na unauthorized mod |
| UI components signature | Ed25519 podpis bundle JS, weryfikacja przy install. Iframe sandbox dla high-risk |

---

## 12. Wydajność i SLO

| Metryka | Target |
|---------|--------|
| Latencja detekcja → alarm (D2 broń/agresja) | < 1.5 s p95 |
| Latencja detekcja → alarm (D1, D3, D6) | < 3 s p95 |
| FPS na kamerze (real-time) | 5–15 dla D1/D3/D6; 15–25 dla D2 |
| Kamery na 1× RTX 4070 (mid tier, profil mieszany) | ~8 mixed lub ~16 light |
| GPU utilization target | 60–80% |
| Wyszukiwanie D5 (10M klatek indexed) | < 800 ms p95 |
| Re-id query D4 (100k embeddings) | < 200 ms p95 |
| Model rollback time | < 60 s |
| FP/h/kamera produkcyjny (po kalibracji) | D2 broń <0.2, D2 agresja <0.5, D3 <0.3 |
| `service_call` round-trip overhead | < 5 ms (poza model inference) |
| `stream_next` poll latency | < 1 ms gdy bufor niepusty |
| Audit append latency | < 10 ms (write-ahead) |

**Benchmark CLI:** `tentavision bench --cameras N --profile mixed` → throughput + latencje + budżet VRAM.

**Heavy combo D2+D4+D5 łamie mid tier** bez time-slicingu — UI ostrzega "overprovisioned" przy konfiguracji profili.

---

## 13. Dataset strategy

- **Zbieranie:** każdy deployment ma right-to-collect bucket (z opt-in od klienta + DPIA). Sample sampling per kamera per detektor
- **Labeling:** wbudowane narzędzie w UI (M5 → "label this alarm") + integracja z Label Studio offline; podział train/val/test stratyfikowany per site
- **Negative examples:** hard-negatives mining z FP alarmów (operator klika "fałszywy" → ląduje w training set)
- **Drift detection:** monthly job — porównuje rozkład embeddingów / scores z baseline; alarm DPO przy drift > threshold
- **Per-site calibration:** każdy deployment ma własne thresholdy + adaptacja per godzina (rano/popołudnie/noc)
- **Retraining pipeline:** offline (gpu-host klienta lub sidecar), nie blokuje produkcji; nowy model → A/B shadow → przełączenie z rollback gotowym

---

## 14. Evaluation harness

Wbudowany w runtime, uruchamiany automatycznie + on-demand:

- **Per-domain P/R/F1** na walidacyjnym secie deployment-specific
- **FP per hour per camera** (alert fatigue — krytyczna metryka)
- **Subgroup metrics** (RODO fairness): performance per płeć, wiek, oświetlenie, pora dnia
- **Latency histograms** per operator (p50/p95/p99)
- **GPU utilization breakdown** per model
- **AI Act post-market monitoring:** automatyczny raport miesięczny w formacie Annex IV — wysyłany do DPO

**CLI:** `tentavision eval --profile <id> --period 7d`

---

## 15. Roadmap implementacyjny F0–F10

| Faza | Zakres | Kryterium zamknięcia |
|------|--------|----------------------|
| **F0** — Plan + research | Plan v0.4 (ten dok), research SDK, mockupy M1–M16, decyzje | Akceptacja użytkownika |
| **F1** — SDK BLOCKER APIs | API-1 (service_call), API-1a/b/c (aliasy + UI M16), API-2 (storage manifest), API-3 (SQL host fn), API-4b (per-addon FS), API-5 (Camera), API-6 (Streaming), API-10 (UI components z sandboxem) | Test addon TentaVision-MVP może utworzyć alias, połączyć się z kamerą RTSP, dostać frame_refs, wywołać yolo przez alias, zapisać do SQL |
| **F2** — TentaVision MVP D1 | Manifest finalny, addon WASM szkielet, M1+M2+M3+M14 ekrany, services yolo+ocr w Dockerze, D1 ADR end-to-end | 1 kamera → ADR check → alarm w M5 |
| **F3** — D3 luggage + Profile + Strefy | M4 profile + Flow selection, M5 alarm center, M9 strefy, D3 luggage Flow + services | 4 kamery z 3 profilami, D3 wykrywa luggage z ABODA |
| **F4** — Search D5 + Vector | API-7 (Vector), API-8 (Recording), M6 wyszukiwarka, services siglip2+lprnet | Search po atrybutach na 24h nagrań |
| **F5** — D2 anomalie | Services videomae-v2 + weapons, M5 z workflow potwierdzania | 3 poddomeny D2 z site-calibrated FP <5% |
| **F6** — Legal gates + Audit + RODO | API-13 (audit risk_class), API-15 (policy/claims engine), M7 D4 gate, M10 audit+RODO, M11 evidence + HSM/TSA, M13 profil prawny | Komercja-profil blokuje D4, Służby z claim pozwala z audit do WORM |
| **F7** — D4 produkcja | Services adaface + transreid, vector namespaces faces/persons, D4 query tylko z aktywnym LegalGrant | Re-id działa tylko z grantem |
| **F8** — Connectory vendors | Hikvision ISAPI, Dahua CGI, Axis VAPIX+ACAP, Hanwha, Bosch, UniFi Protect P2 (z RTSP fallback), Milestone import | 4+ vendory z auto-discovery |
| **F9** — Flow templates + Generic install wizard | API-11 (flow_invoke), API-12 ([[flow_template]]), API-14 (on_install hook), M15 install wizard polished | Generic wizard działa dla TentaVision + min. 1 innego addona |
| **F10** — Scale & Edge | TensorRT/OpenVINO/Jetson edge deployment, multi-node load balance, model rollback <60s, advanced eval harness | Jetson POC + 2-node cluster |

---

## 16. Otwarte pytania i decyzje do podjęcia

1. **Per-addon Postgres vs SQLite default** — czy MVP F1 puszczamy z samym SQLite (prostsze), czy od razu PostgreSQL connection manager?
2. **Strategy weighted dla aliasów** — czy `weighted` jest potrzebne w MVP, czy wystarczy `first_available` + opcjonalnie `round_robin`?
3. **Anchoring BTC** — czy potrzebne dla MVP czy dopiero F10? OpenTimestamps daje 24h opóźnienie potwierdzenia
4. **Migracja SQLite → Postgres** — automatyczny tool czy manual? (M14 ma button "Migrate" — implementacja TBD)
5. **Custom components signing** — czy podpis TentaFlow corp signer wystarczy, czy multi-vendor signing (np. user może akceptować addon signers)
6. **Real-time push do UI** — dziś on_tick + pull. Czy SSE / WebSocket bridge w F3 czy później?
7. **PostgreSQL collation dla "ansi" dialect** — `LIKE` vs `ILIKE`, sort case-sensitivity — może wymagać addon decyzji
8. **Camera ownership cross-addon** — TentaVision dodaje kamerę, MeetingBot chce snapshot. Czy `shared_with` w `cameras` table wystarczy, czy potrzebny generic resource ACL w core?

---

## 17. Glossary

| Termin | Definicja |
|--------|-----------|
| **Addon** | Pakiet WASM + manifest + assets zainstalowany w TentaFlow przez admina |
| **Alias** | Wpis w globalnej tabeli `model_aliases` TentaFlow: `(alias, target_model, fallback_targets, strategy, is_active)`. Addon woła `service_call(alias, ...)`, router rozwiązuje na konkretny target |
| **Application** (tryb addona) | Addon z UI w shellu TentaFlow (entry_panel w manifeście) |
| **ClipRef** | Opaque uchwyt do segmentu nagrania w core recording manager. Addon nigdy nie zna ścieżki pliku |
| **Capabilities** | Cechy serwisu (akceptowane wejście, output, GPU requirements, classes). Używane do filter Flow w M4 |
| **Claim** | Zapis w policy engine: approval / grant / deployment_profile. Wymagane przez gate'y |
| **D1..D6** | Sześć domen analitycznych TentaVision (ADR / Behavior / Luggage / Re-ID / Attributes / Generic) |
| **DPIA** | Data Protection Impact Assessment (RODO art. 35) |
| **FRIA** | Fundamental Rights Impact Assessment (AI Act art. 27) |
| **FrameRef** | Opaque uchwyt do ramki wideo w core streaming. Addon przekazuje do `service_call`, serwis pobiera bajty z core |
| **Flow** | Pipeline DAG w FlowBuilder TentaFlow. TentaVision dostarcza Flow blocks + szablony |
| **Flow block** | Element DAG-a Flow. TentaVision rejestruje `addon.tentavision.adr_check`, ... |
| **Gate** | Polityka wymagająca claims do uruchomienia operacji klasy C |
| **HSM** | Hardware Security Module (Yubikey HSM2 / SoftHSM) — podpis Ed25519 dla evidence |
| **LegalGrant** | Claim typu grant z authority + case_no + expiry — wymagany dla D4 |
| **Manifest** | `manifest.toml` w bundle addona — deklaracja wszystkiego: permissions, aliases, gates, storage, UI components |
| **Mockup M1..M16** | 16 ekranów zaprojektowanych UI (15 TentaVision + 1 systemowy M16) |
| **Owner** (aliasu) | Kto utworzył alias: `addon:<id>` lub `manual` |
| **Service** | Zarejestrowany w TentaFlow Docker service z modelem AI (np. yolo11m-detector). Addon nie widzi services bezpośrednio, tylko przez aliasy |
| **service_call** | Host function — addon woła `service_call(alias, method, payload)` |
| **Strategy** | Algorytm router'a TentaFlow dla aliasów: `first_available` / `round_robin` / `weighted` |
| **Suggested default** | Pole w manifeście `[[alias]]` — addon sugeruje który model powinien być primary target. Admin może zmienić w M16 |
| **TentaFlow** | Główna platforma — host dla addonów |
| **TentaVision** | Ten addon — analiza obrazu z kamer |
| **TSA** | Time Stamping Authority (RFC 3161) dla evidence |
| **WORM** | Write Once Read Many — bucket immutable do externalizacji audit log |
