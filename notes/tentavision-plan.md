# TentaVision — plan addona analizy obrazu z kamer (v0.6)

**Wersja:** v0.6.0 · model dwukierunkowych uprawnien (uses_alias/uses_model + visibility + consumers); rollback runtime alias CRUD ABI; alias.manage → alias.read.

**Poprzednia:** v0.5.3 · po trzecim review codex — naprawione 4 ostatnie sprzeczności: stare frame_<token> przykłady → nowe raw_ref, M16 jasny split F1a basic (text input) vs F2 polished (service_list dropdown), ABI versioning bez back-compat shims (zgodnie z project rules), TEAMS_BOT migracja jednorazowa, F1a estymata realistyczna 10-16 tygodni z milestone split

**Wcześniejsza:** v0.5.2 · po drugim review codex + naprawy nice-to-have (D/B/H z drugiego review): rozbudowane migracje DB z method/request_id/node_id w alias_calls, snapshot before/after w model_alias_changes, multi-signature + revocation w policy_claims, gate_check_cache, frame_pickup_log, addon_migrations_applied. ABI rozszerzone o storage_stats, frame_url, service_list/get, node_resources_get, audit_query/export/verify, evidence_config_get, claim_*, gate_*, flow_*. Versioning ABI, payload limits, semantyka out_cap retry, kompletna lista error codes (0-24). Test matrix rozszerzony o security tests (pickup token replay/TTL/cross-service, path traversal, FS isolation, DoS), UI e2e dla wszystkich mockupów, performance benchmarks.
**Forma:** addon-aplikacja TentaFlow (tryby: application + service tick + tools + flow blocks)
**Deployment:** on-premise (serwery klienta), opcjonalnie multi-node
**Historia:**
- `tentavision-plan-history-v0.1-v0.3.4.md` — iteracyjne wersje
- `tentavision-plan-history-v0.4.md` — pierwsza konsolidacja
- `tentavision-sdk-research.md` — analiza SDK z cytatami kodu
- Mockupy: `~/.gstack/projects/Slyb00ts-TentaFlow/designs/tentavision-v1/` (16 ekranów M1–M16)

## Zmiany v0.5.2 (nice-to-have z drugiego codex review)

| Obszar | Zmiana |
|--------|--------|
| Migracje DB §6.5 | Dodane: `method/request_id/node_id/service_id/payload_bytes/error_code` w `alias_calls`. `before_snapshot/after_snapshot` w `model_alias_changes`. Pełna `policy_claims` z `parent_claim_id`, `revoked_at/reason`, multi-signature przez `policy_claim_signatures`. Nowe: `addon_migrations_applied`, `gate_check_cache`, `frame_pickup_log`. Indeksy pod realne query (alias by addon+ts, claims by expiry+active, frame pickup by service+ts) |
| ABI §6.2 rozszerzone | Dodane host functions: `storage_stats`, `frame_url`, `camera_discover`, `camera_test_connection`, `service_list`/`service_get`, `node_resources_get`, `audit_query`/`export`/`verify`, `evidence_config_get`, `audit_log_with_risk`, `claim_*`/`gate_*`, `flow_invoke`/`status`/`list`/`get` |
| Ogólne reguły ABI | Versioning (`_v1` suffix, manifest sdk_version), payload limits (8MB service_call, 4MB SQL, 1MB vector item), `out_cap` retry semantics (max 1 retry), timeouts per category, kompletna lista 24 error codes |
| Test matrix §17 | Security tests (pickup token replay/TTL/cross-service/forge, frame URL signing, path traversal FakeFile+FS sandbox+SQL attached DB, per-addon FS isolation, SQL injection, quota KV/Vector/SQL/stream, DoS service_call flood + recording, migration partial fail/hash modify/existing DB, FrameRef leak/cross-addon/cross-service, audit chain tamper, claim revocation propagation). UI e2e dla M1-M16. Performance benchmarks z konkretnymi targetami |

## Zmiany v0.5.1 (drobne fixe po drugim review codex)

| Obszar | Zmiana |
|--------|--------|
| Liczba permissions | `18 sztuk` w komentarzu → `22 sztuk` (faktycznie tyle jest) |
| FrameRef | Rozdzielenie na `RawFrameRef` (addon-side, bez tokenu) i `PickupToken` (service-side, scoped per service_call, one-shot, HMAC) — §6.4 |
| Frame URL dla UI | Osobny mechanizm `frame_url(raw_ref, ttl)` (multi-use, signed URL dla `<img>/<video>`) — nie używa `pickup_token` |
| M16 w F1a vs F2 | F1a daje **basic** M16 (lista + edycja primary target jako string). F2 dodaje `service_list` dropdown z autocomplete |
| AI Act timeline | Doprecyzowane: 2.08.2026 to tylko **transparency**; biometric high-risk **2.12.2027**; embedded products **2.08.2028** |

## Zmiany vs v0.4 (po pierwszym review codex)

| Obszar | Zmiana |
|--------|--------|
| Roadmap F1 | Podzielony na **F1a / F1b / F1c** — pierwotny F1 był 3-5 faz w jednej |
| Camera ingest w F1 | F1a używa **fake camera / file replay**; prawdziwe RTSP/ONVIF w F1b |
| Custom UI components | Przesunięte z F1 do **F1c** (po podstawowym MVP) |
| Recording API priorytet | Z F2 → **F1a** (M1/M2/M5 używają już w MVP) |
| PostgreSQL | Wyrzucony z F1; SQLite-only do F4. Pełen PG jako opcjonalny F8 |
| Manifest permissions | Ujednolicone z istniejącym kodem: `secrets.*` (nie `secret.*`), `events.*`, `ui.render`. Dodane `audit.*`, `claim.*`, `alias.manage` |
| Liczba permissions | Skorygowana w komentarzu manifestu (18, nie 14) |
| `fallback_targets` | JSON array only (zgodnie z kodem `routing/middleware.rs:93`) |
| `tv-results-grid` | Dodany do `[[ui_component]]` (M6 go używa) |
| `[[gate]]` priority | Mockupy M7 i M10 zakładają dział funkcji wcześniej niż API-15 F3 — wprowadzono **F2 placeholder API-15a** (proste claims store) |
| `weighted` strategy | Wyrzucone z MVP — tylko `first_available` + `round_robin` w F2 |
| BTC anchoring, ONNX upload, model rollback | Wyrzucone do F10 (były zaśmiecaniem MVP) |
| API-14 (`on_install`) | Usunięte — TentaVision nie potrzebuje, plan sam to przyznawał |
| AI Act timeline | Dodana data **2 grudnia 2027** dla biometric high-risk (część obszarów Annex III) — zmiana po porozumieniu Komisji z 7 maja 2026 |
| ABI kontrakty | Sekcja **§6.2** — pełne sygnatury, JSON schemas, error codes dla każdej F1a host function |
| Service-to-Core API | Sekcja **§6.3** — jak service-side pobiera bajty po `FrameRef` |
| FrameRef security | Sekcja **§6.4** — scoped token, TTL, replay protection, node locality |
| Migracje DB | Sekcja **§6.5** — `model_alias_owners`, `alias_calls`, `model_alias_changes`, `policy_claims_v0` |
| Decyzje techniczne | Sekcja **§16** — camera ingest backend, model_aliases vs service_aliases, permission naming, FrameRef lifecycle |
| Test matrix | Sekcja **§17** — co testujemy w F1a (unit, integration, fake camera, WASM ABI, UI API) |

---

## Spis treści

1. Wizja i scope
2. Architektura systemowa
3. Komponenty
4. Mockupy M1–M16 — opis realizacji
5. Manifest TentaVision (naprawiony)
6. SDK API gaps + ABI kontrakty + migracje
7. Modele AI
8. Connectory kamer
9. Dane (SQL schema, vector, recording)
10. RODO / AI Act / claims-based gates
11. Bezpieczeństwo
12. Wydajność i SLO
13. Dataset strategy
14. Evaluation harness
15. Roadmap implementacyjny F0–F10 (z F1 split)
16. Decyzje techniczne
17. Test matrix dla F1a
18. Otwarte pytania (zredukowane)
19. Glossary

---

## 1. Wizja i scope

TentaVision to addon-aplikacja TentaFlow analizująca strumienie wideo z kamer IP w czasie rzeczywistym i z historii. Jest pełnoprawną aplikacją osadzoną w shellu TentaFlow — admin TentaFlow instaluje ją z marketplace, addon żyje wewnątrz globalnej nawigacji (Addons → TentaVision), korzysta z core API TentaFlow (storage, aliasy AI, kamery, streaming, recording, evidence) i nie dotyka bezpośrednio żadnego hardware ani sieci kamerowej.

### 1.1 Sześć domen analitycznych

| ID | Domena | Tryb | Krytyczność | Klasa RODO/AI Act |
|----|--------|------|-------------|---------------------|
| D1 | ADR — kontrola tablic chemicznych na cysternach | real-time przy bramach/dokach | wysoka | A (bezosobowe) |
| D2 | Anomalie zachowań — upadek, agresja, wandalizm, broń | real-time | krytyczna | B (anonimowa sylwetka) |
| D3 | Pozostawiony bagaż — peron, lotnisko, hala | real-time + post-event | krytyczna | A/B |
| D4 | Re-identyfikacja osób (twarz + person re-id, gait) | post-event pod legal gate; real-time tylko LEA | wysoka | **C — AI Act high-risk** |
| D5 | Wyszukiwanie po atrybutach, tablice, marki/kolory | post-event | średnia | B |
| D6 | Generic object detection | real-time, opt-in | niska | A |

### 1.2 Zasady projektowe

- **Profil analityczny per kamera + harmonogram dzień/noc** — nie wszystko jednocześnie
- **Pipeline = Flow w FlowBuilder TentaFlow**, nie wewnętrzny graf w UI addona; addon dostarcza bloki + szablony Flow
- **Modele AI = aliasy w globalnej tabeli `model_aliases`**; addon deklaruje, admin konfiguruje w systemowym UI
- **Operator I linii widzi maskowane twarze**; unmask wymaga claim
- **D4 (klasa C) zablokowane** dopóki addon nie dostanie kompletu claims
- **Audit hash-chain do WORM** dla każdego query klasy B/C, każdego unmask, każdego eksportu

### 1.3 Co NIE jest scope

- ❌ Bezpośredni RTSP/ONVIF/Protect z addona — to core (camera ingest)
- ❌ Własna inferencja na GPU — wszystko przez aliasy
- ❌ Ramki wideo / klipy w storage addona — recording API zwraca opaque `clip_ref`
- ❌ Konfiguracja mapowania alias → konkretny model w UI addona — to robi admin w globalnym UI TentaFlow
- ❌ Generyczny vector / blob / message-queue / FS poza tym co wystawia core SDK
- ❌ Wybór polityki retencji globalnie — addon deklaruje klasy danych, core enforce

---

## 2. Architektura systemowa

```
┌─ TENTAFLOW CORE (host) ──────────────────────────────────────────────────┐
│                                                                            │
│  ┌─ Addon WASM: TentaVision ────────────────────────────────────────┐   │
│  │  application UI (M1..M13)  · ui_render tree                       │   │
│  │  service tick (250 ms)     · drenaż streamów + agregacja          │   │
│  │  tools (LLM)               · 5 narzędzi                            │   │
│  │  flow blocks               · D1/D2/D3/D5 jako bloki w FlowBuilder │   │
│  │                                                                     │   │
│  │  ↓ host functions (każda permission-check + audit-log)              │   │
│  │  Istniejące (już w core):                                           │   │
│  │    storage_get/set, secret_get/set, llm_*, event_*, ui_*,           │   │
│  │    http_request, log_*, user_*, network_*, oauth_*                   │   │
│  │  Nowe (do dopisania, §6):                                           │   │
│  │    service_call (rozszerzenie), alias_create/get/deactivate,         │   │
│  │    sql_exec/query, camera_*, stream_*, vector_*, recording_*,        │   │
│  │    evidence_*, flow_invoke, claim_add/check, audit_log_with_risk    │   │
│  └────────────────────────────────────────────────────────────────────┘   │
│                                                                            │
│  ┌─ Core moduły (nowe i istniejące) ───────────────────────────────┐    │
│  │  service registry         · routuje service_call po aliasach     │   │
│  │  router (model_aliases)   · resolves alias → target+fallback     │   │
│  │  camera-ingest module     · RTSP/ONVIF/Protect → frame_refs      │   │
│  │  recording manager        · ring-buffer, retencja, signed URLs   │   │
│  │  vector store             · embedded HNSW per addon/namespace    │   │
│  │  evidence signer          · HSM (Yubikey HSM2) + TSA RFC 3161    │   │
│  │  flow runner              · wywołuje Flow z FlowBuilder          │   │
│  │  audit chain              · append-only + hash-chain + WORM      │   │
│  │  policy/claims engine     · gates dla operacji klasy C           │   │
│  │  frame_ref token issuer   · scoped tokens, TTL, audit            │   │
│  └────────────────────────────────────────────────────────────────────┘   │
│                                                                            │
│  ┌─ UI TentaFlow www ────────────────────────────────────────────────┐   │
│  │  /addons/tentavision/*    · 13 ekranów addona (M1..M13)            │  │
│  │  /addons/tentavision/bindings · M14 readonly bindings + storage    │  │
│  │  /services/aliases        · M16 — globalny UI aliasów              │  │
│  │  /addons/*/install        · M15 wizard instalacji (generic)        │  │
│  └────────────────────────────────────────────────────────────────────┘   │
└────────────────────────────────────────────────────────────────────────────┘
            │ QUIC                │ QUIC                  │ QUIC
            ▼                     ▼                       ▼
   ┌─ Service A ──┐      ┌─ Service B ──┐         ┌─ Service C ──┐
   │ yolo11m-     │      │ ppocrv5-ocr   │         │ siglip2-vit  │
   │ detector     │      │ (Docker, GPU) │         │ (Docker, GPU)│
   └──────────────┘      └───────────────┘         └──────────────┘
                                                            ▲
                                                            │ Service-to-Core API
                                                            │ pickup_frame(ref, token)
                                                            ▼
                                                  core frame storage (shared mem)
```

### 2.1 Ścieżki danych (kluczowe przepływy)

**Klatka z kamery → detekcja → alarm:**

1. Core `camera-ingest` trzyma sesję RTSP z C-01
2. Addon woła `stream_subscribe(target=Camera{id:C-01, sample_fps:5}, filter)` → `StreamId`
3. W `on_tick` (co 250 ms) `stream_next(id, timeout=200)` → `Frame{camera_id, ts, frame_ref, sequence}`
4. Addon woła `service_call("tentavision-yolo", "detect", payload{frame_ref, classes:["truck"]})`. Router rozwiązuje alias na `yolo11m-detector @ node-gpu-A`. Serwis YOLO wywołuje **Service-to-Core API** (§6.3): `pickup_frame(frame_ref, token)` → dostaje bajty ramki. Zwraca bboxy
5. Dla D1: addon woła `service_call("tentavision-ocr", "recognize_cropped", payload{frame_ref, region:bbox})` → tekst tablicy
6. Walidacja ADR w addonie (lokalna tabela 2025 jako bundled asset)
7. Jeśli alarm: `sql_exec("INSERT INTO alarms ...")` lokalnie + `event_publish("alarm.created", payload)` + `flow_invoke("tv-alarm-enrich", {alarm_id})`

**Wyszukiwanie historyczne (D5):**

1. User w M6 wpisuje "czerwona czapka, okulary"
2. `service_call("tentavision-vlm", "embed", {text:query})` → wektor 768D
3. `vector_search("attributes", vector, k=30, filter)` → top-30 hits z metadata
4. Dla każdego hit `recording_save_snapshot(camera_id, ts)` lub `recording_get_url(snapshot_ref, ttl=3600)` → miniatura
5. Render grid (M6)

**Eksport dowodowy (M11):**

1. User wybiera alarmy + sygnaturę sprawy
2. Addon zbiera `clip_refs` z `recordings_meta` + snapshots + manifest_json
3. `evidence_sign(payload)` → core składa ZIP, HSM podpisuje, TSA timestamp
4. Wynik: `SignedPackage{id, bundle_url, signature, ...}` — link do download

### 2.2 Aliasy AI — model przetwarzania

Aliasy żyją w globalnej tabeli `model_aliases` TentaFlow (potwierdzone w `tentaflow-core/src/db/migrations.rs:225`). Schema: `(alias UNIQUE, target_model, is_active, fallback_targets, strategy)`.

| Rola | Co robi |
|------|---------|
| **Addon** | Przy aktywacji woła `alias_create(spec)` dla każdego z `[[alias]]` w manifest. `spec.suggested_default` → `target_model` (lub pusty). Aliasy z `gate` tworzone z `is_active=0`. Przy dezaktywacji → `alias_deactivate(id)` (NIE usuwa) |
| **Admin TentaFlow** | W globalnym UI **Serwisy → Aliasy** (M16) widzi wszystkie aliasy, edytuje `target_model`, `fallback_targets` (drag-to-reorder), `strategy`, `is_active` |
| **Router (core)** | Przy każdym `service_call` rozwiązuje alias wg strategii. Dla `first_available`: primary, przy `down` → fallback. Zwraca `ServiceResponse{payload, executed_by, duration_ms, fallback_used}` |

Wzorzec potwierdzony w teams-bot: `tentaflow-core/src/addon/mod.rs:1890` — `TEAMS_BOT_ALIASES` (hard-coded dziś, plan: przenieść do `[[alias]]` w manifeście).

---

## 3. Komponenty TentaVision

### 3.1 Addon WASM (bundle)

| Plik | Co zawiera |
|------|------------|
| `manifest.toml` | Pełna deklaracja (§5) |
| `tentavision.wasm` | Kompilowany Rust → WASM (wasm32-wasip1) |
| `migrations/001_init.sql`, `002_*.sql` | Schema SQL (uruchamiane przez core przy install/upgrade, kolejność leksykograficzna) |
| `flows/tv-*.flow.json` | Szablony Flow do opt-in importu w FlowBuilder |
| `components/tv-*.js` | Custom web components (signed Ed25519, F1c) |
| `assets/adr-table-2025.json` | Tabela klas ADR jako bundled asset |
| `assets/legal/*.md` | Szablony klauzul informacyjnych, DPIA/FRIA |

Tryby aktywne równolegle:

| Tryb | Manifest | Cel |
|------|----------|-----|
| **Application** | `[application] entry_panel = "dashboard"` | 13 ekranów M1..M13 |
| **Service tick** | `[service] tick_interval_ms = 250` | Drenaż `stream_next`, agregacja KPI |
| **Tools (LLM)** | `[[tool]]` × 5 | `search_attribute`, `check_adr`, `confirm_alarm`, `run_flow`, `export_evidence` |
| **Flow blocks** | `blocks.json` | `addon.tentavision.adr_check`, ... |

### 3.2 Serwisy AI (Docker) — zewnętrzne wobec addona

Zarejestrowane w TentaFlow service registry niezależnie od TentaVision. Lista wymaganych w deployment:

| Service | Funkcja | HW |
|---------|---------|----|
| yolo11m-detector | D1, D6 | GPU 8+GB |
| yolo11s-detector | fallback yolo11m | GPU 4+GB lub CPU |
| ppocrv5-ocr | D1 OCR | GPU 4+GB |
| parseq-adr | fallback OCR | GPU |
| videomae-v2-rwf2k | D2 | GPU 12+GB |
| weapons-yolo-fine-tune | D2 broń | GPU |
| siglip2-vit-l14 | D5 atrybuty | GPU 8+GB |
| adaface-r100 | D4 face | GPU 4+GB |
| transreid | D4 person re-id | GPU |
| lprnet-pl | D5 tablice | GPU/CPU |

### 3.3 Core moduły TentaFlow (mapowanie na F1a/F1b/F1c/F2/...)

| Moduł | Wystawia API | Priorytet | Status w core dziś |
|-------|--------------|-----------|---------------------|
| Service call (rozszerzony) | `service_call(alias, method, payload)` | **F1a** | `service_request` istnieje, brak `method` |
| Alias mgmt (WASM ABI) | `alias_create/deactivate/get/list_owned` | **F1a** | `repository::create_or_reactivate_model_alias` istnieje, brak WASM ABI |
| Aliases UI (M16) | `/services/aliases` web UI | **F1a** | Brak (CLI/SQL only) |
| SQL backend | `sql_exec/query/transaction`, per-addon SQLite | **F1a** | Brak |
| Per-addon FS sandbox | `~/.tentaflow/addons/<id>/` | **F1a** | Brak |
| Camera (fake/file replay) | `camera_*` z FakeFile connector | **F1a** | Brak |
| Streaming + FrameRef | `stream_subscribe/next/close` | **F1a** | Brak |
| Recording manager (basic) | `recording_save_snapshot/save_segment/get_url` | **F1a** | Brak |
| Camera (real RTSP+ONVIF) | RTSP/ONVIF connector | **F1b** | Brak |
| Custom UI components | `[[ui_component]]` + iframe sandbox | **F1c** | Brak |
| Policy/claims (basic) | `claim_add/check/revoke`, audit risk_class | **F2** | Brak |
| Vector store | `vector_upsert/search/delete` | **F2** | Brak |
| Flow invoke | `flow_invoke/flow_status` | **F2** | Częściowo (flow runner istnieje) |
| Evidence signer | `evidence_sign/verify/anchor`, HSM+TSA | **F3** | Brak |
| Camera (vendor connectors) | Hikvision/Dahua/Axis/Hanwha/Bosch/UniFi | **F8** | Brak |
| PostgreSQL backend | Per-addon PG database + role mgmt | **F8** | Brak |
| BTC anchoring | OpenTimestamps integration | **F10** | Brak |

### 3.4 UI TentaFlow www

| Lokalizacja | Co dodajemy | Mockup | Priorytet |
|-------------|-------------|--------|-----------|
| `/addons/tentavision/*` | 13 zakładek (M1–M13) | M1–M13 | F1a–F7 (per feature) |
| `/addons/tentavision/bindings` | M14 readonly view aliasów + storage | M14 | F1a |
| `/addons/{addon-id}/install` | Generic wizard 6-krokowy | M15 | F1a |
| `/services/aliases` | Globalny UI aliasów | M16 | **F1a** (blocker dla TentaVision install) |

---

## 4. Mockupy M1–M16 — opis realizacji

Format: cel · persona · storyboard · pod spodem (API, dane, permissions) · klasa danych.

### M1 — Dashboard

**Cel:** szybki przegląd zdrowia systemu. KPI, ostatnie alarmy, heatmapa aktywności.

**Persona:** operator I linii, analyst, admin.

**Storyboard:** detail-header z chip statusu i badges (22/24 kamer, 8 detektorów, 3 critical, GPU 68%). Action buttons: Odśwież / Eksport / Live view. Tabs-bar z aktywną "Dashboard" + segmented 1h/6h/24h/7d. 4 KPI tiles. Sekcja "Ostatnie alarmy" (lista z miniaturkami). Sekcja "Stan runtime" (tabela: throughput, queue depth, drop rate, VRAM, modele, clock-sync, audit→WORM, eval). Heatmapa 24h × kamera.

**Pod spodem:**
- KPI: `camera_list({status:online}).count`, `sql_query("SELECT COUNT(*) FROM alarms WHERE ts>?")`, agregat z metadanych `service_call().duration_ms` w KV
- Alarmy: `sql_query("SELECT id, ts, camera_id, detector, status, confidence, clip_ref FROM alarms ORDER BY ts DESC LIMIT 4")`. Miniatura: `recording_get_url(clip_ref, ttl_sec=600)`
- Stan runtime: KV (`storage_get("runtime:*")` zapisywane przez service tick)
- Heatmapa: `sql_query("SELECT camera_id, FLOOR(ts/3600) AS hour, COUNT(*) FROM alarms WHERE ts BETWEEN ? AND ? GROUP BY camera_id, hour")` → grid CSS przez `ui_render`
- Service tick: co 250 ms odświeża KPI, pełny re-render co 5s lub na event
- **Permissions:** `camera.read`, `sql.read`, `recording.read`, `storage.read`

**Klasa danych:** A/B (mix); brak C na ekranie bez claim.

### M2 — Live view (grid kamer)

**Cel:** monitoring real-time wielu kamer z overlay analizy.

**Persona:** operator I linii, analyst.

**Storyboard:** toolbar z togglami overlay (bboxes, etykiety, pose, strefy, mask faces). Segmented 1/4/9/16. Grid kafelków video z overlay top (nazwa, REC), overlay bottom (chip detector, FPS, zegar), bounding box rysowany kolorem (success/warning/danger). Kamery offline z czerwonym overlay.

**Pod spodem:**
- Subskrypcja per kamera: `stream_subscribe(Camera{id, sample_fps:5})` → `StreamId`
- `on_tick` woła `stream_next(id, 200ms)` po wszystkich aktywnych
- **Render obrazu:** `FrameRef` opaque → addon przekazuje do `recording_get_url(frame_ref_as_clip, ttl=60s)` lub specjalny endpoint `frame_url(frame_ref)` (część API-6). `<img src>` w UI tree
- **Bbox overlay:** addon subskrybuje `stream_subscribe(DetectorEvents{profile_id})` per profil → `StreamMessage::Detection{frame_ref, boxes}`
- **Maskowanie twarzy:** w core (camera-ingest lub frame-postprocess), addon nie ma kontroli nad raw obrazem
- **Custom component `tv-video-grid`** (F1c, signed): iframe sandbox, postMessage bridge `{cameras: [{id, frame_url, bboxes, status}]}`. W F1a placeholder zwykłym `UiComponent::Image` + bbox jako `Card` z relative positioning
- Toggle overlay state w KV per user
- **Permissions:** `camera.read`, `stream.subscribe`, `recording.read`

**Klasa danych:** A/B per profil.

### M3 — Kamery — lista & wizard

**Cel:** zarządzanie listą kamer + 4-krokowy wizard dodawania z auto-discovery.

**Persona:** admin TentaVision.

**Storyboard:** detail-header z liczbą kamer, search box, "+Dodaj kamerę". Filter tabs (Wszystkie / Online / Offline / Ostrzeżenia / Niepowiązane). Tabela: Nazwa | Vendor / Protokół | Adres | Status | Profil | FPS | Diagnostyka. Wizard krok 2: lista znalezionych kamer (ONVIF/mDNS/ARP discovery) + formularz poświadczeń + warning o vendor quirks.

**Pod spodem:**
- Lista: `camera_list()` (Camera API) + lokalna replika w SQL `cameras` table
- Auto-discovery: `camera_discover(network_hint, timeout)` (Camera API) — core wykonuje WS-Discovery + mDNS + ARP scan
- Test: `camera_test_connection({vendor, url, credentials_ref})` → capabilities + snapshot URL
- Zapis: `camera_add(spec)` → `CameraId` + `sql_exec("INSERT INTO cameras ...")`
- **Poświadczenia:** addon najpierw `secret_set("cam-<id>-creds", encrypted_blob)` → `SecretRef` → przekazuje do `camera_add(spec.credentials_secret_ref)`. Addon nigdy nie ma plaintext
- Diagnostyka: background tick `camera_health(id)` co 30s → `sql_exec("UPDATE cameras SET ...")`
- **W F1a:** dostępny tylko `FakeFileCamera` (replay z mp4); F1b dodaje RTSP+ONVIF; F8 dodaje Hikvision/Dahua/Axis/Hanwha/Bosch/UniFi
- **Permissions:** `camera.manage`, `secrets.write`, `sql.write`

**Klasa danych:** A (metadata kamer). Credentials = secrets (klasa zgodna z polityką org).

### M4 — Profile analityczne

**Cel:** profil = `{cel, Flow, kamery, harmonogram, retencja, quick params}`. Builder = wybór Flow + slidery (NIE wewnętrzny graf).

**Persona:** analyst, admin.

**Storyboard:** detail-header z nazwą profilu i risk class. Sub-tabs (Graf operatorów aktywny / Harmonogram / Strefy / Akcje&reguły / Kamery / Historia). Karta Flow z metadanymi + dropdown "Zmień Flow" (filtrowana lista). Quick params (slidery: legibility threshold, FPS, latency budget, klasy obiektów). Konfiguracja (nazwa, cel, harmonogram, retencja, tier QoS). Capabilities readonly. Tabela "Profile w deployment".

**Pod spodem:**
- Profile w SQL: `profiles(id, name, flow_id, schedule, retention, data_class, active, quick_params_json)`
- Lista Flow: `flow_list(filter:{requires_capabilities:[...]})` (API-11). Filtrowanie po `requires_capabilities` Flow
- Quick params jako Flow inputs override przy `flow_invoke(flow_id, params)`
- Capabilities mapping per Flow: `flow_get(flow_id).capabilities` zwraca jakie aliasy Flow używa wewnątrz; UI pokazuje per capability mapping w `model_aliases`
- Test na nagraniu: `flow_invoke_with_input(flow_id, recorded_clip_ref)` (F4)
- **Permissions:** `sql.read/write`, `flow.invoke`, `camera.read`

**Klasa danych:** A.

### M5 — Centrum alarmów

**Cel:** real-time feed alarmów z filtrami + szczegółowa karta z klipem 30s + workflow potwierdzania.

**Persona:** operator I linii, analyst.

**Storyboard:** detail-header z liczbą critical/warning. Split layout: lewa kolumna (380px) feed alarmów; prawa kolumna detail (klip video, timeline 10 klatek, metadane, workflow Potwierdź/Fałszywy/Eskaluj, notatka).

**Pod spodem:**
- Feed: subskrypcja `stream_subscribe(EventBus{topic:"alarm.created"})` → push do UI; inicjalny load `sql_query("SELECT * FROM alarms WHERE status='pending' ORDER BY ts DESC LIMIT 50")`
- Detail: `sql_query_one("SELECT * FROM alarms WHERE id=?")`
- Klip 30s: `recording_save_segment(camera_id, alarm.ts-5s, alarm.ts+25s)` → `ClipRef` (cache w `alarms.clip_ref`). `recording_get_url(clip_ref, ttl=3600)` → `<video src>`
- Timeline: `recording_save_snapshot(camera_id, alarm.ts+offset)` × 10 → snapshots
- Workflow: `sql_exec("UPDATE alarms SET status=?, operator_id=?, ...")` + `event_publish("alarm.confirmed", {alarm_id})` + audit log z risk_class
- Eskalacja: `flow_invoke("tv-alarm-escalate", {alarm_id})`
- **Permissions:** `sql.read/write`, `recording.read/save`, `stream.subscribe`, `events.publish`

**Klasa danych:** A/B/C zależnie od detektora. Klipy klasy C wymagają unmask + claim.

### M6 — Wyszukiwarka historyczna

**Cel:** post-event search: tekst (CLIP/SigLIP), atrybut, podobieństwo (zdjęcie), tablica rejestracyjna.

**Persona:** analyst.

**Storyboard:** 4 karty modes. Form: query + filter (kamery, czas, klasy). Wyniki: 5-kolumnowy grid result-card z bbox + score. Footer warning RODO.

**Pod spodem:**
- Indeksowanie (background): service tick co N sek dla profili D5/D6 → `service_call("tentavision-vlm", "embed", {frame_ref})` → `vector_upsert("attributes", [...])`
- Query tekstowe: `service_call("tentavision-vlm", "embed", {text:query})` → `vector_search("attributes", query_vec, k=30, filter:{camera_id IN [...], ts BETWEEN ...})`
- Query po obrazie: upload → `service_call("tentavision-vlm", "embed", {image_bytes})` → vector_search
- Query po tablicy: `service_call("tentavision-yolo", "detect", {classes:["license_plate"]})` + walidator PL/EU lokalnie, lub `service_call("tentavision-lprnet", "recognize")` → `vector_search("plates", ...)`
- Render: dla każdego hit `recording_save_snapshot(camera_id, ts)` → custom component `tv-results-grid` (zadeklarowany w `[[ui_component]]`)
- Audit: każde wyszukanie → `sql_exec("INSERT INTO search_audit ...")` + `audit_log_with_risk(action, risk_class:"B")`
- Eksport: `evidence_sign({clip_refs: hits, manifest_json: {query, results}})`
- **Permissions:** `vector.read`, `service.call`, `recording.read`, `sql.write`

**Klasa danych:** B. Faces/persons (C) tylko przez M7.

### M7 — Re-ID (D4) — pod legal gate

**Cel:** wyszukiwanie po twarzy/sylwetce, **twardo zablokowane** dopóki nie spełnione claims.

**Persona:** analyst-lea, DPO, supervisor uprawniony.

**Storyboard:** czerwone obramowanie, big-ico, risk C, chip "zablokowany". Gate modal z checklist 6 warunków (DPIA✓, FRIA warning, LegalGrant blocked, profil deployment blocked, audit hash-chain✓, post-market✓). Po unblock: tabela indeksu `subj-XXXX`.

**Pod spodem:**
- Gate check: `gate_check("d4-historical")` → status każdego claim. Core czyta z `policy_claims`
- LegalGrant request: formularz → `claim_add({type:"grant", scope:"biometric:historical", authority, case_no, expiry, ...})` + workflow DPO+supervisor
- Zmiana profilu: link do M12 → `claim_add({type:"deployment_profile", value:"lea"})` z DPO sign
- Po unlock — indeks: `sql_query("SELECT * FROM legal_grants WHERE expiry>NOW() AND scope LIKE 'biometric:%' AND is_active=1")`
- Query D4 face: `service_call("tentavision-face-embed", "embed", {image_bytes})` → wektor → `vector_search("faces", vec, k=10, filter:{legal_grant_id:?})`. **Każdorazowo `gate_enforce("d4-historical")` przed**. Każdy query → audit z risk_class C + WORM export
- Right to be forgotten: `vector_delete("faces", [subject_id])` + audit
- **Permissions:** `vector.read`, `service.call` (alias musi być `is_active=1` — gate enforcement), audit klasy C automatyczny

**Klasa danych:** C (zawsze).

### M8 — Modele i runtime

**Cel:** stan modeli AI (przez aliasy), VRAM budget, benchmark per kamera. **Uproszczone w F1a/MVP** — bez upload ONNX i rollback (te do F8+).

**Persona:** admin, ML engineer.

**Storyboard:** detail-header z liczbą modeli i GPU. Sekcja VRAM bar. Tabela modeli z wersjami, licencjami, statusem. Sekcja benchmark per kamera (tabela z FPS bar i latency).

**Pod spodem:**
- Lista: `service_list()` (Service Registry API, nowa F2) + filtr po tych co są używane w aliasach z `alias_list_owned()`
- VRAM info: agregat z service metadanych raportowanych do core przy QUIC handshake. Addon czyta przez `node_resources_get(node_id)` (F2)
- Benchmark: background job `tentavision_bench(profile, duration)` zapisuje do KV i SQL `benchmark_runs` table
- **Rollback w F8+:** `service_rollback(service_name, to_version)` — przez core, service-side action
- **Upload ONNX w F8+:** `model_upload(file_bytes, manifest)` — odrębny flow, addon tylko czeka aż admin doda do service registry
- **Permissions:** `service.read` (nowa, readonly)

**Klasa danych:** A.

### M9 — Strefy, harmonogramy, reguły

**Cel:** polygon editor stref na obrazie + kalendarz tygodniowy + reguły AND/OR.

**Persona:** analyst, admin.

**Storyboard:** layout dwóch kolumn (widok kamery z polygons + lista stref), sekcja harmonogram (grid 5h × 7d), tabela reguł kompozytowych.

**Pod spodem:**
- Strefy: `zones(id, camera_id, name, type, polygon_json, color, used_by_detectors)`. polygon_json = `[{x_pct, y_pct}, ...]` (procent obrazu, niezależne od resolution)
- **Edytor (F1c):** custom component `tv-zone-editor` (signed Ed25519, iframe sandbox). Otrzymuje `camera_snapshot(id)` + polygons przez postMessage. Save → addon `sql_exec("UPDATE zones ...")`. **W F1a: placeholder z form do ręcznego wpisania współrzędnych**
- Harmonogram: `sql_query("SELECT * FROM schedules WHERE camera_id=?")` → grid jako `UiComponent::Grid` z kolorowanymi cellami
- Reguły: `rules(id, name, expression, action, active)`. Expression = mini-DSL parsowany w addonie. Eval przy detection events
- **Permissions:** `sql.read/write`, `camera.read`

**Klasa danych:** A.

### M10 — Audyt + RODO

**Cel:** hash-chain log + retencja per klasa + generator DPIA/FRIA.

**Persona:** DPO, admin, audytor.

**Storyboard:** detail-header z statusem hash-chain. Sekcja retencja (4 karty A/B/C/audit). Sekcja log z search box i tabelą wpisów. Sekcja generator dokumentów (3 karty).

**Pod spodem:**
- Hash-chain audit (core): istniejąca `audit_log` + nowa `audit_chain` z Merkle hash chain. API: `audit_query(filter)`, `audit_export(time_range, format)`, `audit_verify(from_hash)`. WORM externalizacja
- Retencja: background job (core) co 1h, usuwa starsze niż retention class. Audit wpisuje fact usunięcia
- DPIA generator: `dpia.assessment` table, auto-wypełnia (kategorie z manifestu, detektory aktywne, kamery, retencja, model versions), user wypełnia (cel, ryzyko, mitigacje) → PDF
- FRIA: analogicznie dla AI Act art. 27
- Klauzula info: szablon PDF z `assets/legal/`
- **Permissions:** `audit.read`, `sql.read/write`

**Klasa danych:** mix; retention rules respektują klasę.

### M11 — Eksport dowodowy

**Cel:** paczki dowodowe signed HSM + TSA dla służb. Lista uprawnionych odbiorców, log eksportów.

**Persona:** admin, supervisor uprawniony, LEA-officer.

**Storyboard:** detail-header z HSM status. Sekcje: Uprawnieni odbiorcy / Łańcuch zaufania / Lista paczek (evidence-card layout).

**Pod spodem:**
- Lista paczek: `sql_query("SELECT * FROM evidence_exports ORDER BY created_at DESC")`. Faktyczne pliki w core evidence storage
- Nowa paczka: wybór alarm-ids + sygnatura + organ → `evidence_sign({clip_refs, snapshots, manifest_json: {legal_grant_id, case_no, authority, scope, requested_by}})` → `SignedPackage{id, bundle_url, signature, timestamp_token, chain_hash}`
- **Anchoring (F10):** co 24h core robi BTC anchor (OpenTimestamps) — `chain_hash` weryfikowalny publicznie
- Verifier CLI: `tentavision verify package.tvevidence` (F8+)
- Uprawnieni odbiorcy: `evidence_recipients(authority, pgp_key, active)` (manage przez admin TentaFlow). Paczka encrypted ich PGP
- Audit: każdy export → hash-chain klasy C + `legal_grant_id` w manifest
- **Permissions:** `evidence.sign` (gate: `deployment_profile_lea_or_critical`), audit klasy C

**Klasa danych:** C.

### M12 — Ustawienia addona

**Cel:** storage limits, retencja, inference backends, powiadomienia, licencje, profil prawny.

**Persona:** admin TentaVision.

**Storyboard:** detail-header z deployment + profil prawny. 4 karty grid 2×2 (Storage / Inference / Powiadomienia / Licencje). Sekcja "Profil prawny & AI Act" z dropdownem (zmiana wymaga DPO sign).

**Pod spodem:**
- Storage settings: KV `storage_set("config:retention:A", "30")` itd. Core czyta przy purge
- Inference backend: wybór wpływa na preferred services (matchowane przez capability)
- Webhook/SMS/Email: każdy target host w `[[network_rule]]`. Secrets przez `secret_set`. **TentaVision manifest deklaruje `webhook-callback` oraz `notifications.twilio` (Twilio API) i `notifications.smtp` (mail)**
- HSM/TSA: configuration core (TentaFlow ma jeden HSM globalnie), addon czyta przez `evidence_config_get()` readonly
- Zmiana profilu prawnego: modal "wymagany DPO sign" → `claim_add({type:"deployment_profile"})` + audit
- **Permissions:** `storage.write`, `secrets.write`, `claim.write`

**Klasa danych:** A.

### M13 — Onboarding (profil prawny)

**Cel:** wybór profilu prawnego determinującego dostępność D4. Część M15 wizard lub osobne pierwsze uruchomienie.

**Persona:** DPO + admin.

**Storyboard:** welcome screen + progress bar. 4 karty profili (Komercja default / Transport / Lotnisko / Służby). Per karta: opis, dostępne detektory (chipy success/warning/danger).

**Pod spodem:**
- Zapis: `claim_add({type:"deployment_profile", value:"commercial"|"transport"|"airport"|"lea"})`. Każdy profil = template defaults (retencja, gates aktywne, generator dokumentów)
- Wpływ na D4: gate `d4-realtime` wymaga `deployment_profile.value IN ["lea","critical_infra"]`
- Audit: zmiana profilu → hash-chain + DPO signature
- **Permissions:** `claim.write` (przez DPO)

**Klasa danych:** A (config), wpływa na C.

### M14 — Bindings & Storage

**Cel:** readonly view aliasów AI utworzonych przez addon + statystyki wbudowanych API.

**Persona:** admin TentaVision, supervisor.

**Storyboard:** detail-header z liczbą aktywnych aliasów. Info banner "readonly + link do M16". Tabela 6 aliasów (alias name | current target | strategy | last used target | status). Sekcja Storage (4 karty: KV / SQL / Vector / Recording). Sekcja SQL content (tabele z rozmiarami). Vector namespaces. 3 karty (Camera / Streaming / Evidence stats).

**Pod spodem:**
- `alias_list_owned()` → `Vec<AliasInfo{id, methods, current_target, fallback_targets, strategy, last_used_target, last_used_at, calls_24h, is_active}>`
- Last used target: router po każdym `service_call` zapisuje który target wykonał. Statystyki w nowej tabeli `alias_calls(alias_id, target_used, ts, duration_ms, fallback_used)` w core (migracja, §6.5)
- KV stats: `storage_stats()` (nowa, F2) → `{key_count, total_bytes, last_modified}`
- SQL stats: `sql_query("SELECT name FROM sqlite_master WHERE type='table'")` + per-table COUNT
- Vector stats: `vector_count(ns)`, `vector_storage_size(ns)` (F2)
- Recording stats: `recording_stats(camera_id)` (F2)
- **Permissions:** `service.read`, `storage.read`, `sql.read`, `vector.read`, `recording.read`, `camera.read`

**Klasa danych:** A (metadata).

### M15 — Install wizard (generic, 6 kroków)

**Cel:** wizard instalacji addona (generic w TentaFlow). TentaVision pokazuje 6 kroków.

**Persona:** admin TentaFlow.

**Storyboard:** header z addon meta, progress bar 6 kroków. Krok 3 aktywny (Aliasy AI). Lista aliasów z manifestu + status (will be created / empty target / inactive gated). Details (collapsible) z poprzednich kroków.

**Pod spodem (core, nie addon):**
- Krok 1 Permissions: parser czyta `[[permission]]` → checkboxes pogrupowane risk-level → admin akceptuje → `addon_permissions(addon_id, permission_id, granted)`
- Krok 2 Storage: jeśli `sql=true` i `sql_backends` ma więcej niż jeden → wybór. SQLite → tworzy `~/.tentaflow/addons/<id>/`. **W F1a tylko SQLite**, PG w F4
- Krok 3 Aliasy: czyta `[[alias]]`, sprawdza collision. Po finalizacji `create_or_reactivate_model_alias(alias, suggested_default, "first_available")` z `is_active = !gate_required`
- Krok 4 Flow templates: preview każdego `[[flow_template]]` → user wybiera import → `flows` tabela core
- Krok 5 Profil prawny: M13 inline
- Krok 6 Pierwsza kamera: M3 wizard inline (opcjonalny)
- Po finalizacji: addon zaktywowany, service tick startuje

**Klasa danych:** A.

### M16 — Serwisy → Aliasy (globalny systemowy UI)

**Cel:** systemowe UI w sekcji Services TentaFlow do konfiguracji wszystkich aliasów.

**Persona:** admin TentaFlow.

**Storyboard:** sidebar TentaFlow z aktywnym Services, breadcrumb /Services/Aliasy. Tabs (Wszystkie serwisy / Modele / Aliasy aktywny / Węzły / Historia). Filter chips. Tabela aliasów 7 kolumn. Inline edit dialog dla wybranego aliasu (primary target + strategy radio + fallback chain builder z drag-to-reorder + metadata + Save/Delete).

**F1a basic (M16 v1)** vs **F2 polished (M16 v2):**

| Funkcja | F1a (M16 v1) | F2 (M16 v2) |
|---------|---------------|--------------|
| Lista wszystkich aliasów | ✓ | ✓ |
| Filter chips (owner, status, strategy) | ✓ | ✓ |
| Edit `is_active` toggle | ✓ | ✓ |
| Edit `fallback_targets` (JSON array, drag-to-reorder) | ✓ | ✓ |
| Edit `strategy` (radio first_available / round_robin) | ✓ (first_available default) | ✓ (+round_robin) |
| Edit `target_model` primary | **Text input (admin wpisuje string)** | **Dropdown z `service_list()` autocomplete** |
| `weighted` strategy | ❌ wycofane MVP | ❌ |
| Manual alias creation | ✓ | ✓ |

**Pod spodem (core, nie addon TentaVision):**
- Lista: `SELECT * FROM model_aliases ORDER BY alias` + dla każdego count z `alias_calls` (migracja §6.5)
- Owner: tabela `model_alias_owners(alias_id, owner_type, owner_id)`. Wartości: `manual` lub `addon:<addon_id>`
- Edit primary F1a: text input. F2: dropdown z `service_list()` (API-1c+)
- Strategy: `first_available` (F1a) / `round_robin` (F2)
- Fallback chain: JSON array w `model_aliases.fallback_targets` (kod `routing/middleware.rs:93`)
- Drag-to-reorder: SortableJS w obu wersjach
- Save: `UPDATE model_aliases SET ...` + audit `model_alias_changes` z `before_snapshot/after_snapshot` (migracja §6.5)
- Delete: addon-owned → tylko `is_active=0`. Manual → DELETE
- **Permissions:** rola admin TentaFlow (nie addon-level)

**Klasa danych:** A.

---

## 5. Manifest TentaVision (draft v0.4 — naprawiony)

Naprawy względem v0.4:
- Ujednolicone **`secrets.*`** (nie `secret.*`) — zgodnie z teams-bot `manifest.toml`
- Ujednolicone **`events.publish/subscribe`** (kropka, nie underscore)
- Dodane **`audit.read`**, **`audit.write_classC`**, **`claim.write`**, **`alias.manage`**, **`service.read`**, **`stream.subscribe`** (były wymagane przez mockupy a brakowały)
- Dodany **`tv-results-grid`** w `[[ui_component]]` (używany w M6)
- Komentarz "18 sztuk" w `[[permission]]` zgodnie z faktyczną liczbą
- `signature` z **placeholder** wyjaśnionym jako "wypełniane przy packaging, Ed25519 nad bundle JS"
- Dodane sekcje sieciowe Twilio + SMTP (używane w M12)

```toml
# manifest.toml — TentaVision v0.1.0

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
tick_interval_ms = 250
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
sql_backends = ["sqlite"]               # F1a tylko SQLite; PG w F8
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
suggested_default = ""                   # auto_bind po deployu silnika

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

# === Permissions (22 sztuk) =======================================
# Naming zgodne z istniejącym kodem (secrets.*, events.*, ui.render)
[[permission]]
id = "service.call"
display_name = "Wywołaj zarejestrowane services przez aliasy"
description = "service_call(alias, method, payload)"
risk = "medium"

[[permission]]
id = "service.read"
display_name = "Czytaj listę services i metadane (readonly)"
description = "service_list, node_resources_get dla M8"
risk = "low"

[[permission]]
id = "alias.manage"
display_name = "Utwórz/dezaktywuj aliasy AI w globalnej tabeli"
description = "alias_create, alias_deactivate, alias_get"
risk = "medium"

[[permission]]
id = "camera.manage"
display_name = "Dodaj/usuń/konfiguruj kamery"
description = "camera_add, camera_update, camera_remove"
risk = "medium"

[[permission]]
id = "camera.read"
display_name = "Czytaj listę i metadane kamer"
risk = "low"

[[permission]]
id = "stream.subscribe"
display_name = "Subskrybuj strumienie z kamer"
description = "stream_subscribe(Camera|Detector|EventBus)"
risk = "medium"

[[permission]]
id = "sql.read"
description = "sql_query, sql_query_one"
risk = "low"

[[permission]]
id = "sql.write"
description = "sql_exec, sql_transaction"
risk = "low"

[[permission]]
id = "vector.read"
description = "vector_search, vector_count"
risk = "low"

[[permission]]
id = "vector.write"
description = "vector_upsert, vector_delete"
risk = "low"

[[permission]]
id = "recording.save"
display_name = "Zapisuj klipy z ring-buffera"
description = "recording_save_segment, recording_save_snapshot"
risk = "medium"

[[permission]]
id = "recording.read"
description = "recording_get_url, recording_stats"
risk = "medium"

[[permission]]
id = "evidence.sign"
display_name = "Podpisz paczki dowodowe (HSM)"
description = "evidence_sign — wymaga gate deployment_profile_lea_or_critical"
risk = "high"
gate = "deployment_profile_lea_or_critical"

[[permission]]
id = "events.publish"
description = "Emit alarm.*, recording.* events"
risk = "low"

[[permission]]
id = "events.subscribe"
description = "Subscribe to event bus topics"
risk = "low"

[[permission]]
id = "flow.invoke"
description = "flow_invoke, flow_status"
risk = "medium"

[[permission]]
id = "secrets.read"
description = "secret_get — credentials kamer, tokeny"
risk = "high"

[[permission]]
id = "secrets.write"
description = "secret_set — credentials kamer, tokeny"
risk = "medium"

[[permission]]
id = "audit.read"
description = "audit_query — dla M10 audit viewer"
risk = "low"

[[permission]]
id = "audit.write_classC"
display_name = "Wpisuj do audit log z risk_class=C"
description = "audit_log_with_risk dla D4 query, unmask, evidence sign"
risk = "high"

[[permission]]
id = "claim.write"
display_name = "Utwórz/odwołaj claims (legal grants, approvals)"
description = "claim_add, claim_revoke — przez DPO/supervisor"
risk = "high"

[[permission]]
id = "ui.render"
description = "ui_render(panel_id, tree)"
risk = "low"

# === Network rules ================================================
[[network_rule]]
id = "webhook-callback"
protocol = "tcp"
host = "*.tentaflow.local"
port = 443
description = "Webhook callback do flow-engine TentaFlow"
required = false

[[network_rule]]
id = "notifications-twilio"
protocol = "tcp"
host = "api.twilio.com"
port = 443
description = "SMS notifications (opcjonalnie, gdy admin skonfiguruje)"
required = false

[[network_rule]]
id = "notifications-smtp"
protocol = "tcp"
host = "smtp.*"
port = 587
description = "Email notifications (opcjonalnie)"
required = false

# === Flow templates (opt-in install) ==============================
[[flow_template]]
id = "tv-realtime-adr"
display_name = "Real-time analiza ADR"
path = "flows/tv-realtime-adr.flow.json"
description = "frame → yolo (vehicle) → yolo (plate) → ocr → legibility → ADR validator → event"

[[flow_template]]
id = "tv-alarm-enrich"
display_name = "Wzbogacenie alarmu"
path = "flows/tv-alarm-enrich.flow.json"
description = "save clip → snapshots → vlm embed → store → notify"

[[flow_template]]
id = "tv-evidence-export"
display_name = "Eksport dowodowy"
path = "flows/tv-evidence-export.flow.json"
description = "zbierz clipy + snapshots + grant → evidence_sign → bundle"

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
param_type = "string"            # 'confirm'|'reject'|'escalate'
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
description = "Wygeneruj podpisaną paczkę dowodową"
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

# === Custom UI components ========================================
# Signature: wypełniane przy packaging tools (Ed25519 nad bundle JS,
# weryfikowane przez core przy install).
[[ui_component]]
id = "tv-video-grid"
display_name = "Grid kamer z bbox overlay"
slot = "main"
src = "components/tv-video-grid.js"
signature = "ed25519:<base64-signature-placeholder>"
risk = "high"                   # iframe sandbox

[[ui_component]]
id = "tv-zone-editor"
display_name = "Edytor stref polygonowych"
slot = "main"
src = "components/tv-zone-editor.js"
signature = "ed25519:<base64-signature-placeholder>"
risk = "high"

[[ui_component]]
id = "tv-heatmap"
display_name = "Heatmapa aktywności 24h"
slot = "main"
src = "components/tv-heatmap.js"
signature = "ed25519:<base64-signature-placeholder>"
risk = "low"

[[ui_component]]
id = "tv-results-grid"
display_name = "Grid wyników wyszukiwarki (M6)"
slot = "main"
src = "components/tv-results-grid.js"
signature = "ed25519:<base64-signature-placeholder>"
risk = "medium"

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

## 6. SDK API gaps + ABI kontrakty + migracje

### 6.1 Lista API z priorytetami (po split F1)

| API ID | Funkcja | Priorytet | Kod dziś |
|--------|---------|-----------|-----------|
| API-1 | `service_call(alias, method, payload) → ServiceResponse` | **F1a** | `service_request` istnieje (bez method, payload pakowany w `CompletionPayload`) — `host_functions/service.rs:31-220` |
| API-1a | `alias_create / alias_deactivate / alias_get / alias_list_owned` | **F1a** | Repository istnieje (`mod.rs:1890`), brak WASM ABI |
| API-1b | `[[alias]]` w manifeście | **F1a** | Brak — hard-coded w core (`mod.rs:1880` TEAMS_BOT_ALIASES) |
| API-1c | UI `/services/aliases` (M16 basic — tylko lista i edycja primary target dla aliasów już istniejących) | **F1a** | Brak (CLI/SQL only) |
| API-1c+ | M16 z `service_list` dropdown (autocomplete dla primary target z zarejestrowanych services) | **F2** | W F1a admin wpisuje target ręcznie jako string (znając nazwy services) |
| API-2 | `[storage]` w manifeście + per-addon storage paths | **F1a** | Brak |
| API-3 | `sql_exec/query/transaction` host functions | **F1a** | Brak |
| API-4 | Migrations runner | **F1a** | Brak |
| API-4b | Per-addon FS sandbox (`~/.tentaflow/addons/<id>/`) | **F1a** | Brak |
| API-5 | Camera API (z FakeFile connector w F1a, RTSP/ONVIF w F1b) | **F1a/F1b** | Brak |
| API-6 | Streaming API + FrameRef opaque | **F1a** | Brak |
| API-8 (basic) | Recording: `save_segment`, `save_snapshot`, `get_url` (M1/M2/M5 wymagają) | **F1a** | Brak |
| API-10 | Custom UI components z signature + iframe sandbox | **F1c** | Brak |
| API-15a | Claims store basic + `gate_check`/`gate_enforce` (audit risk_class) | **F2** | Brak |
| API-7 | Vector API | **F2** | Brak |
| API-11 | Flow invoke (`flow_invoke/flow_status/flow_list`) | **F2** | Częściowo (Flow runner istnieje) |
| API-12 | `[[flow_template]]` opt-in install | **F2** | Brak |
| API-13 | Audit `risk_class` enum + tagging | **F2** | Brak |
| API-8 (full) | Recording: ring-buffer with retention policies | **F3** | Brak |
| API-9 | Evidence: `evidence_sign/verify/anchor` z HSM/TSA | **F3** | Brak |
| API-15 (full) | Generic policy/claims engine z workflow approvals | **F3** | Brak |
| API-3b | PostgreSQL backend + per-addon role | **F8** | Brak |
| API-9b | BTC anchoring (OpenTimestamps) | **F10** | Brak |
| **Usunięte** | `weighted` strategy aliasów | n/a | Wycofane z MVP |
| **Usunięte** | `on_install` lifecycle hook (API-14) | n/a | TentaVision nie używa |
| **Usunięte** | Model rollback / ONNX upload w F1 | F8+ | Zbyt złożone do MVP |

### 6.2 ABI kontrakty dla F1a host functions

Każda host function ma: nazwa, parametry (ptr/len wg WASM ABI), input/output JSON schema, error codes, audit format, permission required.

#### service_call

**WASM ABI:**
```
service_call(
  alias_ptr: i32, alias_len: i32,         # nazwa aliasu
  method_ptr: i32, method_len: i32,       # nazwa metody
  payload_ptr: i32, payload_len: i32,     # payload jako bajty (typically JSON)
  out_ptr: i32, out_cap: i32,             # bufor na response
  out_len_ptr: i32,                       # *u32 — ile zapisane
) -> i32                                   # ABI_OK / ABI_ERR_*
```

**Input JSON schema (payload, dla method="detect"):**
```json
{
  "raw_ref": "frame_<uuid>",
  "node_id": "node-a",
  "classes": ["truck", "person"],
  "confidence_min": 0.5
}
```
*(`raw_ref` to `RawFrameRef.raw_ref` z `stream_next` — sam bez tokenu. Core przy resolve `service_call` wystawi `PickupToken` dla service-side — zob. §6.4)*

**Output JSON schema (response wrapped):**
```json
{
  "payload": <bytes/json — service-specific>,
  "executed_by": "yolo11m-detector",
  "duration_ms": 12,
  "fallback_used": false,
  "request_id": "req_<uuid>"
}
```

**Error codes:**
- `ABI_OK = 0`
- `ABI_ERR_PERMISSION = 1` — brak `service.call`
- `ABI_ERR_NOT_FOUND = 2` — alias nie istnieje albo `is_active=0` albo gate niezaspokojony
- `ABI_ERR_NO_AVAILABLE_TARGET = 3` — primary + wszystkie fallback down
- `ABI_ERR_TIMEOUT = 4` — service nie odpowiedział w czasie
- `ABI_ERR_OPERATION = 5` — service zwrócił błąd
- `ABI_ERR_OUTPUT_BUFFER_TOO_SMALL = 6` — out_cap za małe

**Audit format:**
```
addon_id=tentavision, action=service.call, alias=tentavision-yolo, method=detect,
executed_by=yolo11m-detector, duration_ms=12, fallback_used=false,
result=ok|denied|error, error_message=?
```

**Permission:** `service.call`.

#### alias_get_v1 / alias_list_owned_v1 (readonly)

Addon **nie tworzy ani nie deaktywuje** aliasow w runtime. Tworzenie i
deaktywacja aliasow odbywa sie wylacznie w lifecycle hooks core
(install/uninstall/upgrade — zob. §6.6 oraz `ADDON_MANIFEST.md` sekcja
`[[alias]]`). Host functions widoczne dla addona to wylacznie readonly.

**ABI (`alias_get_v1`):**
```
alias_get_v1(
  alias_id_ptr: i32, alias_id_len: i32,
  out_ptr: i32, out_cap: i32, out_len_ptr: i32,
) -> i32
```

`alias_get_v1` zwraca `AliasInfo` (TOML). Pola statystyczne
(`last_used_target`, `last_used_at`, `calls_24h`, `fallback_calls_24h`) sa
stripowane gdy `caller_addon_id != owner_id` aliasu — chroni przed wyciekiem
wzorcow uzycia miedzy addonami.

```toml
id = "tentavision-yolo"
display_name = "Detektor obiektów (D1, D6)"
owner = "addon:tentavision"
visibility = "private"
current_target = "yolo11m-detector"
fallback_targets = ["yolo11s-detector", "yolo11n-cpu"]
strategy = "first_available"
is_active = true
# nizsze tylko gdy caller == owner
last_used_target = "yolo11m-detector"
last_used_at = 1715515200
calls_24h = 3414
fallback_calls_24h = 8
```

**ABI (`alias_list_owned_v1`):**
```
alias_list_owned_v1(
  out_ptr: i32, out_cap: i32, out_len_ptr: i32,
) -> i32
```

Zwraca TOML array `AliasInfo` — tylko aliasy z `owner_id = <this_addon_id>`.

**Errors:** `ABI_OK` (0), `ABI_ERR_PERMISSION` (1), `ABI_ERR_NOT_FOUND` (2),
`ABI_ERR_OUTPUT_BUFFER_TOO_SMALL` (6).

**Permission:** `alias.read` (uprzednio `alias.manage` — przemianowane w v0.6).

#### sql_exec / sql_query / sql_query_one / sql_transaction

**sql_exec ABI:**
```
sql_exec(
  query_ptr: i32, query_len: i32,        # SQL string
  params_json_ptr: i32, params_json_len: i32,  # JSON array []Value
  out_ptr: i32, out_cap: i32, out_len_ptr: i32,
) -> i32
```

**Output (sql_exec):**
```json
{ "rows_affected": 1, "last_insert_id": "alarm_42" }
```

**sql_query** zwraca:
```json
{
  "columns": ["id", "ts", "camera_id"],
  "rows": [
    ["alarm_42", 1715515200, "C-04"],
    ["alarm_41", 1715515100, "C-07"]
  ]
}
```

**Value types (JSON):** string, integer (i64), real (f64), boolean, null, base64-encoded bytes (`{"$bytes": "..."}`).

**Errors:**
- `ABI_ERR_SQL_SYNTAX = 8`
- `ABI_ERR_SQL_CONSTRAINT = 9` — violated UNIQUE/FK/etc.
- `ABI_ERR_SQL_NO_RESULT = 10` — query_one zwraca brak
- `ABI_ERR_QUOTA_EXCEEDED = 11`

**Restrictions:**
- DDL (CREATE/ALTER/DROP) blocked from runtime — tylko przez migrations
- Parameters required (no string concat w core)
- Query timeout 30s default

**Audit:** writes: `addon_id, action=sql.exec, query_hash, rows_affected`. Reads: sample audit (co N-ty).

**Permission:** `sql.read` / `sql.write`.

#### camera_add / camera_list / camera_get / camera_snapshot / camera_health

**camera_add ABI:**
```
camera_add(
  spec_json_ptr, spec_json_len,
  out_ptr, out_cap, out_len_ptr,
) -> i32
```

**Input:**
```json
{
  "vendor": "fake_file",                  // F1a: tylko "fake_file"; F1b: + rtsp, onvif
  "location": "Brama wjazdowa",
  "url_or_host": "/test/sample.mp4",      // dla fake_file: ścieżka do pliku replay
  "credentials_secret_ref": "secret_<uuid>",
  "retention_class": "A",
  "ownership": "tentavision",
  "shared_with": []
}
```

**Output:**
```json
{ "camera_id": "cam_<uuid>", "capabilities": {...} }
```

**camera_snapshot** zwraca:
```json
{ "image_ref": "img_<token>_<uuid>", "ttl_sec": 600 }
```

Z `image_ref` można pobrać URL przez `recording_get_url(image_ref, ttl)`.

**camera_health:**
```json
{
  "status": "online|offline|degraded",
  "last_seen": 1715515200,
  "fps_actual": 5,
  "fps_target": 5,
  "flags": ["clock_drift", "image_dark"],
  "drop_rate_1h": 0.004
}
```

**Errors:**
- `ABI_ERR_CAMERA_UNREACHABLE = 12`
- `ABI_ERR_CAMERA_AUTH_FAILED = 13`
- `ABI_ERR_CAMERA_VENDOR_UNSUPPORTED = 14`

**Permission:** `camera.manage` / `camera.read`.

#### stream_subscribe / stream_next / stream_close

**stream_subscribe ABI:**
```
stream_subscribe(
  target_json_ptr, target_json_len,       # StreamTarget
  filter_json_ptr, filter_json_len,       # StreamFilter
  out_ptr, out_cap, out_len_ptr,
) -> i32
```

**Target types:**
```json
{ "type": "camera", "camera_id": "cam_<uuid>", "sample_fps": 5 }
{ "type": "detector_events", "profile_id": "prof_<uuid>" }
{ "type": "event_bus", "topic_pattern": "alarm.*" }
```

**Output:**
```json
{ "stream_id": "stream_<uuid>", "buffer_size": 100 }
```

**stream_next ABI:**
```
stream_next(
  stream_id_ptr, stream_id_len,
  timeout_ms: i32,
  out_ptr, out_cap, out_len_ptr,
) -> i32
```

**Output (jeśli message available):**
```json
{
  "type": "frame|event|detection|drop|end",
  "camera_id": "cam_<uuid>",
  "ts": 1715515200,
  "sequence": 12345,
  "frame_ref": "frame_<token>_<uuid>",      // dla type=frame|detection
  "boxes": [...],                           // dla type=detection
  "event_kind": "...",                      // dla type=event
  "event_payload": {...},                   // dla type=event
  "drop_count": 12,                         // dla type=drop
  "drop_reason": "buffer_full"
}
```

Jeśli timeout: zwraca `ABI_OK` z `out_len = 0` (no message).

**Errors:**
- `ABI_ERR_STREAM_NOT_FOUND = 15`
- `ABI_ERR_STREAM_CLOSED = 16`
- `ABI_ERR_BACKPRESSURE = 17` — bufor pełen, core pozbawił najstarszych

**Permission:** `stream.subscribe`.

#### recording_save_snapshot / recording_save_segment / recording_get_url

**recording_save_segment ABI:**
```
recording_save_segment(
  camera_id_ptr, camera_id_len,
  start_ts: i64, end_ts: i64,
  out_ptr, out_cap, out_len_ptr,
) -> i32
```

**Output:**
```json
{
  "clip_ref": "clip_<uuid>",
  "duration_ms": 30000,
  "size_bytes": 28500000,
  "hash_sha256": "..."
}
```

**recording_get_url ABI:**
```
recording_get_url(
  ref_ptr, ref_len,                       # clip_ref lub snapshot_ref
  ttl_sec: i32,
  out_ptr, out_cap, out_len_ptr,
) -> i32
```

**Output:**
```json
{
  "url": "https://tentaflow.local/recording/clip_<uuid>?token=...",
  "expires_at": 1715518800
}
```

URL jest signed (HMAC), TTL-limited. Po expiry — 403.

**Errors:**
- `ABI_ERR_RECORDING_NOT_FOUND = 18`
- `ABI_ERR_RECORDING_PURGED = 19` — retention już usunęło
- `ABI_ERR_RECORDING_TIME_OUT_OF_RING = 20` — żądany czas wykraczał poza ring buffer

**Permission:** `recording.save` / `recording.read`.

#### frame_url (osobny od pickup_token — dla UI)

**ABI:**
```
frame_url(
  raw_ref_ptr, raw_ref_len,               # RawFrameRef
  ttl_sec: i32,                           # max 600 (10 min)
  out_ptr, out_cap, out_len_ptr,
) -> i32
```

**Output:**
```json
{
  "url": "https://tentaflow.local/frame/<raw_ref>?token=<short-hmac>&exp=<ts>",
  "expires_at": 1715515260
}
```

URL signed (HMAC core master key, short version dla browser-friendly), TTL-limited, **multi-use** w obrębie TTL (inaczej niż `pickup_token` który one-shot per service).

**Permission:** `recording.read`.

### 6.2.X Pozostałe host functions — rozszerzony ABI

Dla każdej funkcji: ABI signature + JSON I/O + errors + permission. Krótsze tu, bo wzorce powtarzalne.

#### storage_get / storage_set / storage_delete / storage_list / storage_stats

**Już istnieją w core** (`host_functions/storage.rs`). Dodajemy `storage_stats`:

```
storage_stats(
  out_ptr, out_cap, out_len_ptr,
) -> i32
```

Output: `{ "key_count": N, "total_bytes": N, "last_modified": ts, "quota_bytes": N }`. Permission: `storage.read`.

#### secret_set / secret_get

**Już istnieją** (`host_functions/secrets.rs`). Plan korzysta as-is dla credentials kamer.

#### camera_discover / camera_test_connection

```
camera_discover(
  network_hint_ptr, network_hint_len,     # CIDR np. "192.168.40.0/24" lub puste = all
  timeout_ms: i32,
  out_ptr, out_cap, out_len_ptr,
) -> i32
```

Output: `{ "discovered": [{ vendor, mac, ip, port, capabilities: {onvif_profiles, has_ptz, ...}, vendor_confidence }] }`.

`camera_test_connection(spec_json) -> CameraCapabilities` — próbny connect bez `camera_add`.

Permission: `camera.manage`.

#### service_list / service_get (dla M16 v2 + M8)

```
service_list(
  filter_json_ptr, filter_json_len,       # { kind, status, node_id, capabilities }
  out_ptr, out_cap, out_len_ptr,
) -> i32
```

Output: `{ "services": [{ id, name, kind, node_id, status, capabilities, registered_at }] }`. **F2** (po F1a basic M16 z manual target text input).

`service_get(id)` zwraca pełną metadata jednego service-u.

Permission: `service.read`.

#### node_resources_get (dla M8)

```
node_resources_get(
  node_id_ptr, node_id_len,
  out_ptr, out_cap, out_len_ptr,
) -> i32
```

Output: `{ "node_id", "gpu": [{model, vram_total_mb, vram_used_mb, utilization}], "cpu": {cores, util}, "ram_mb_total", "ram_mb_used" }`. **F2**. Permission: `service.read`.

#### audit_query / audit_export / audit_verify (dla M10)

```
audit_query(filter_json) -> { entries: [...], count }
audit_export(time_range, format='jsonl'|'csv') -> { url, expires_at }
audit_verify(from_hash) -> { valid: bool, broken_at: hash? }
```

**F2**. Permission: `audit.read`.

#### evidence_config_get (dla M11, readonly)

```
evidence_config_get() -> { hsm_device, tsa_url, anchoring: { type, last_anchor_ts } }
```

Readonly, addon nie konfiguruje. Permission: brak (every addon może czytać dla informacji). **F3**.

#### audit_log_with_risk

Rozszerzenie istniejącego audit_log o explicit risk_class:

```
audit_log_with_risk(
  action_ptr, action_len,
  resource_type_ptr, resource_type_len,
  resource_id_ptr, resource_id_len,
  risk_class_ptr, risk_class_len,         # "A"|"B"|"C"
  related_claim_id_ptr, related_claim_id_len, # opcjonalnie
  result_ptr, result_len,
) -> i32
```

**F2**. Permission: `audit.write_classC` dla risk_class="C", w innych przypadkach automatyczny.

#### claim_add / claim_check / claim_revoke / gate_check / gate_enforce

```
claim_add(spec_json) -> { claim_id, status }
claim_check(claim_id) -> { valid: bool, expires_in_sec, ... }
claim_revoke(claim_id, reason_ptr, reason_len) -> ()
gate_check(gate_id) -> { satisfied: bool, missing_claims: [...] }
gate_enforce(gate_id) -> ()   # returns ABI_ERR_GATE_NOT_SATISFIED jeśli not satisfied
```

**F2**. Permission: `claim.write`.

#### flow_invoke / flow_status / flow_list / flow_get

```
flow_invoke(flow_id, input_json) -> { run_id }
flow_status(run_id) -> { state, output_json?, error? }
flow_list(filter_json) -> { flows: [...] }
flow_get(flow_id) -> { id, name, version, capabilities, inputs, outputs }
```

**F2**. Permission: `flow.invoke`.

### 6.2.Y Ogólne reguły ABI (versioning, payload limits, semantyka out_cap)

**Versioning ABI:**
- Każda host function ma minor version w nazwie eksportu wasmtime (np. `service_call_v1`)
- Major version bump = nowy export, **stara jest usuwana**. Addon dostosowuje się natychmiast (zgodnie z project rules CLAUDE.md: "no backward-compat shims")
- Manifest deklaruje wymaganą wersję SDK: `[addon] sdk_version = ">=0.2.0"`. Core przy install sprawdza compatibility — addon z `sdk_version` niezgodne z core SDK → install rejection

**Payload size limits (per host function):**
- `service_call` payload: max **8 MB** (image bytes, batch inference)
- `sql_exec/query` params + result: max **4 MB** combined
- `vector_upsert` items: max **1 MB** per item, max **1000 items** per call
- `recording_save_segment` no payload (asks core to wykuć segment)
- `secret_set` value: max **64 KB**
- `ui_render` tree: max **2 MB** serialized JSON

Przekroczenie → `ABI_ERR_PAYLOAD_TOO_LARGE = 21`.

**out_cap retry semantics:**
Każda host function która zwraca dane w buforze wyjściowym ma jednolitą semantykę:
1. Caller alokuje bufor o rozmiarze `out_cap` i przekazuje pointer
2. Host function próbuje pisać:
   - Jeśli `actual_size <= out_cap` → zapisuje, ustawia `*out_len_ptr = actual_size`, zwraca `ABI_OK`
   - Jeśli `actual_size > out_cap` → **nie pisze**, ustawia `*out_len_ptr = actual_size` (jak duży bufor jest potrzebny), zwraca `ABI_ERR_OUTPUT_BUFFER_TOO_SMALL`
3. Caller realokuje bufor do `actual_size` (z marginesem +10%) i powtarza wywołanie
4. **Max retry: 1** (drugi `ABI_ERR_OUTPUT_BUFFER_TOO_SMALL` → addon error, audit anomaly)

To pozwala addonowi handle dynamicznie rozmiar odpowiedzi bez over-allocation.

**Timeout defaults:**
- `service_call`: 30 s (configurable per service)
- `sql_*`: 30 s
- `stream_next`: caller-provided
- `recording_*`: 60 s (save może być ciężkie)
- `vector_search`: 5 s
- `camera_*`: 15 s
- `flow_invoke`: returns immediately (RunId), `flow_status` poll z TTL 5 min

**Error code numbering:**
```
0  = ABI_OK
1  = ABI_ERR_PERMISSION
2  = ABI_ERR_NOT_FOUND
3  = ABI_ERR_NO_AVAILABLE_TARGET
4  = ABI_ERR_TIMEOUT
5  = ABI_ERR_OPERATION (generic)
6  = ABI_ERR_OUTPUT_BUFFER_TOO_SMALL
7  = ABI_ERR_CONFLICT
8  = ABI_ERR_SQL_SYNTAX
9  = ABI_ERR_SQL_CONSTRAINT
10 = ABI_ERR_SQL_NO_RESULT
11 = ABI_ERR_QUOTA_EXCEEDED
12 = ABI_ERR_CAMERA_UNREACHABLE
13 = ABI_ERR_CAMERA_AUTH_FAILED
14 = ABI_ERR_CAMERA_VENDOR_UNSUPPORTED
15 = ABI_ERR_STREAM_NOT_FOUND
16 = ABI_ERR_STREAM_CLOSED
17 = ABI_ERR_BACKPRESSURE
18 = ABI_ERR_RECORDING_NOT_FOUND
19 = ABI_ERR_RECORDING_PURGED
20 = ABI_ERR_RECORDING_TIME_OUT_OF_RING
21 = ABI_ERR_PAYLOAD_TOO_LARGE
22 = ABI_ERR_GATE_NOT_SATISFIED
23 = ABI_ERR_FRAME_TOKEN_INVALID
24 = ABI_ERR_FRAME_PURGED
```

### 6.3 Service-to-Core API (jak service-side pobiera bajty po FrameRef)

Service-side (Docker container yolo11m-detector, ppocrv5-ocr, ...) wykonuje:

```
HTTP/QUIC: POST /core/frame/pickup
Headers:
  Authorization: Bearer <service_token>           # token QUIC auth do core
  X-Pickup-Token: <pickup_token>                  # one-shot HMAC wystawiony przy service_call (§6.4)
  X-Frame-Raw-Ref: frame_<uuid>                   # raw_ref bez tokenu — selektor
  X-Service-Id: yolo11m-detector
  X-Request-Id: req_<uuid>

Response 200 OK:
  Content-Type: image/jpeg | image/png | video/mp4 (segment)
  X-Frame-Width: 1920
  X-Frame-Height: 1080
  X-Frame-Codec: jpeg|h264
  X-Frame-Pts: 1715515200000
  X-Camera-Id: cam_<uuid>
  Body: <bajty>

Response 403: invalid token / service not authorized for this frame_ref
Response 404: frame_ref purged from ring buffer
Response 410: frame_ref TTL expired
```

**Mechanizm:**
- Core przy każdym `service_call(alias, method, payload)` emituje token scoped do tej konkretnej operacji:
  - `frame_ref` w payload → core generuje `service_token` ważny 30s, scoped do `(service_id, frame_ref, request_id)`
  - Token + request_id → przekazywane do service przez QUIC payload
  - Service używa do pobrania bajtów
- Po użyciu (lub TTL expiry) token unieważniony

**Bajty ramki:** core trzyma w shared memory (per-node) z LRU. Recording manager osobno zapisuje to disk dla `recording_save_*`.

**FrameRef format:** `frame_<scope_token>_<uuid>` — addon dostaje nie-scoped ref, dopiero `service_call` go scoped per service.

### 6.4 FrameRef security model — dwa odrębne typy

**Kluczowe rozdzielenie** (wyklarowane po codex review v0.5):

#### `RawFrameRef` — uchwyt po stronie addona

```json
{
  "raw_ref": "frame_<uuid>",
  "node_id": "node-a",
  "camera_id": "cam_<uuid>",
  "ts": 1715515200,
  "sequence": 12345
}
```

- Wystawiany przez `stream_next` do addona
- **Nie zawiera tokenu** — to tylko identyfikator + metadane
- Sam w sobie **nieużyteczny** do pobrania bajtów (nawet jeśli wyciekający z addona)
- Addon przekazuje `raw_ref` jako część payload do `service_call`
- Addon może bez problemu zapisać `raw_ref` do SQL (np. `alarms.frame_ref`) — to nie sekret

#### `PickupToken` — scoped credential dla service-side

Wystawiany **w core przy każdym `service_call(alias, method, payload{raw_ref})`**:

```
PickupToken = HMAC(core_master_key, {
  raw_ref,
  service_id,        // konkretny target po router resolve (yolo11m-detector)
  request_id,        // UUID nowy per service_call
  expiry: now+30s,
  one_shot: true
})
```

Core przekazuje token do service przez QUIC payload (sąsiad `raw_ref`-u). Service używa do `pickup_frame`:

```
HTTP/QUIC: POST /core/frame/pickup
  X-Frame-Ref: <raw_ref>
  X-Pickup-Token: <token>
  X-Service-Id: yolo11m-detector
  X-Request-Id: <request_id>
```

Core weryfikuje token (HMAC + scope match) → zwraca bajty + invalidate token. Drugi pickup z tym samym tokenem → 403.

#### Tabela mechanizmów

| Aspekt | Mechanizm |
|--------|-----------|
| Wytwarzanie `raw_ref` | `stream_next` (core streaming bus) |
| Wytwarzanie `PickupToken` | `service_call` (core router) — jeden per service_call |
| TTL token | 30s default (configurable `[stream] pickup_token_ttl_ms`) |
| TTL `raw_ref` (bajty w shared mem) | LRU, default 1024 ramki per node |
| Replay protection | `one_shot = true` — invalidate po użyciu |
| Node locality | Token ma `node_id` — pickup tylko z tego node (multi-node) |
| Multi-service same frame | Każde `service_call` daje **nowy token** dla tej samej `raw_ref` — addon może wywołać `service_call("yolo")` + `service_call("ocr")` na tej samej ramce, każdy z osobnym tokenem |
| Cross-addon | Token scoped do calling addon_id — addon A nie może użyć tokenu z addon B |
| Authorization | Service musi być zarejestrowany; addon nie ma access do `pickup_frame` (audit anomaly jeśli próbuje) |
| Cleanup | Background co 60s purge expired tokens. `stream_close` → invalidate raw_refs + tokens |
| Audit | Każdy `pickup_frame` logowany (service, raw_ref, request_id, ts, result) |

#### Frame URL dla UI (osobny mechanizm)

Live view (M2) potrzebuje URL do `<img>` lub `<video>`. To nie jest `pickup_token` (który jest one-shot per service). Mechanizm:

- `frame_url(raw_ref, ttl_sec) → Url` — **nowa host function** (część API-6)
- Core wystawia signed URL `https://tentaflow.local/frame/<raw_ref>?token=<short-hmac>&exp=<ts>`
- URL jest **multi-use** w obrębie TTL (bo browser może wielokrotnie pobierać)
- Default TTL: 60s
- Wymaga permission `recording.read` (bo to de facto frame retrieval)
- **Inaczej niż `pickup_token`** — addon-controlled, nie service-side
- Audit: wystawienie URL logowane, retrieval przez frontend nie (zbyt częste)

### 6.5 Migracje DB potrzebne dla F1a/F2 (rozszerzone v0.5.2)

Pliki migracji w `tentaflow-core/src/db/migrations.rs`. Każda migracja ma jednoznaczny numer + nazwa + sprawdzenie idempotentne.

```sql
-- ============================================================================
-- F1a — owner aliasów
-- ============================================================================
CREATE TABLE model_alias_owners (
  alias_id INTEGER PRIMARY KEY REFERENCES model_aliases(id) ON DELETE CASCADE,
  owner_type TEXT NOT NULL CHECK(owner_type IN ('addon', 'manual')),
  owner_id TEXT,                          -- addon_id lub null dla manual
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_alias_owners_addon ON model_alias_owners(owner_type, owner_id);

-- ============================================================================
-- F1a — statystyki użycia aliasów (dla M14, M16) — z pełnymi fields
-- ============================================================================
CREATE TABLE alias_calls (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  alias_id INTEGER NOT NULL REFERENCES model_aliases(id) ON DELETE CASCADE,
  alias_name TEXT NOT NULL,               -- denormalized dla szybkich query (alias może być usunięty)
  method TEXT,                            -- "detect", "recognize", "embed" itd.
  target_used TEXT NOT NULL,              -- konkretny service po router resolve
  target_node_id TEXT,                    -- na jakim nodzie execute
  service_id TEXT,                        -- ID serwisu z registry (jeśli zmapowany 1:1)
  caller_addon_id TEXT,
  caller_user_id INTEGER,                 -- przez którego usera triggered (jeśli applicable)
  request_id TEXT,                        -- UUID per service_call (correlation z audit + pickup_frame)
  duration_ms INTEGER,
  payload_bytes INTEGER,                  -- rozmiar input (dla quota/billing)
  response_bytes INTEGER,
  fallback_used INTEGER DEFAULT 0,
  fallback_chain_position INTEGER,        -- 0=primary, 1=first fallback, 2=second, ...
  result TEXT NOT NULL CHECK(result IN ('ok', 'error', 'no_target', 'timeout', 'permission_denied', 'gate_denied')),
  error_code TEXT,                        -- np. "ABI_ERR_TIMEOUT" jeśli error
  ts INTEGER NOT NULL
);
CREATE INDEX idx_alias_calls_alias_ts ON alias_calls(alias_id, ts);
CREATE INDEX idx_alias_calls_addon_ts ON alias_calls(caller_addon_id, ts);
CREATE INDEX idx_alias_calls_request_id ON alias_calls(request_id);
CREATE INDEX idx_alias_calls_fallback ON alias_calls(alias_id, fallback_used) WHERE fallback_used=1;

-- ============================================================================
-- F1a — change log dla M16 audit (snapshot before/after + change_type)
-- ============================================================================
CREATE TABLE model_alias_changes (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  alias_id INTEGER NOT NULL,              -- może być po DELETE, nie FK
  alias_name TEXT NOT NULL,               -- denormalized
  changed_by_user_id INTEGER,
  changed_by_addon_id TEXT,
  before_snapshot TEXT,                   -- pełny JSON snapshot wiersza model_aliases przed
  after_snapshot TEXT,                    -- pełny JSON po (NULL dla DELETE)
  change_type TEXT NOT NULL CHECK(change_type IN
    ('create','target_change','fallback_change','strategy_change',
     'activate','deactivate','delete','suggested_default_change')),
  reason TEXT,                            -- opcjonalny komentarz admina
  ts INTEGER NOT NULL
);
CREATE INDEX idx_alias_changes_alias ON model_alias_changes(alias_id);
CREATE INDEX idx_alias_changes_user_ts ON model_alias_changes(changed_by_user_id, ts);

-- ============================================================================
-- F1a — tabela wersji migracji per addon
-- ============================================================================
CREATE TABLE addon_migrations_applied (
  addon_id TEXT NOT NULL,
  migration_name TEXT NOT NULL,           -- np. "001_init.sql"
  migration_hash TEXT NOT NULL,           -- SHA256 treści — wykrycie modyfikacji
  applied_at TEXT NOT NULL DEFAULT (datetime('now')),
  applied_in_addon_version TEXT NOT NULL, -- np. "0.1.0"
  status TEXT NOT NULL CHECK(status IN ('success', 'failed', 'partial')),
  error_message TEXT,
  duration_ms INTEGER,
  PRIMARY KEY (addon_id, migration_name)
);
CREATE INDEX idx_addon_migrations_status ON addon_migrations_applied(addon_id, status);

-- ============================================================================
-- F2 — policy claims (rozszerzone: multi-signature, revocation chain)
-- ============================================================================
CREATE TABLE policy_claims (
  id TEXT PRIMARY KEY,                    -- UUID
  claim_type TEXT NOT NULL CHECK(claim_type IN ('approval','grant','deployment_profile','consent')),
  subject TEXT,                           -- approval: 'dpia','fria'; grant: subject_id/case_id
  scope TEXT NOT NULL,                    -- "biometric:historical", "biometric:realtime", itd.
  value TEXT,                             -- dla deployment_profile: 'lea','critical_infra',...
  status TEXT NOT NULL CHECK(status IN ('draft','pending','signed','revoked','expired')),
  expiry INTEGER,                         -- NULL = no expiry (deployment_profile)
  has_expiry INTEGER NOT NULL DEFAULT 0,  -- denormalized dla index
  authority TEXT,                         -- "Prokuratura Rejonowa Warszawa-Mokotów"
  case_no TEXT,                           -- "PR-3-K-247/2026"
  requested_by_user_id INTEGER,
  is_active INTEGER NOT NULL DEFAULT 1,
  scope_addon_id TEXT,                    -- claim per-addon lub global (NULL)
  audit_chain_hash TEXT,                  -- link do hash-chain audit
  parent_claim_id TEXT,                   -- dla revocation chain (np. revoke wskazuje co revoke)
  created_at INTEGER NOT NULL,
  revoked_at INTEGER,
  revoked_reason TEXT
);
CREATE INDEX idx_claims_type_scope ON policy_claims(claim_type, scope);
CREATE INDEX idx_claims_expiry ON policy_claims(expiry, is_active) WHERE has_expiry=1;
CREATE INDEX idx_claims_scope_addon ON policy_claims(scope_addon_id) WHERE scope_addon_id IS NOT NULL;
CREATE INDEX idx_claims_authority_case ON policy_claims(authority, case_no);
CREATE INDEX idx_claims_status_active ON policy_claims(status, is_active);

-- Multi-signature: per claim może być kilka podpisów (DPO + supervisor + ...)
CREATE TABLE policy_claim_signatures (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  claim_id TEXT NOT NULL REFERENCES policy_claims(id) ON DELETE CASCADE,
  signer_user_id INTEGER NOT NULL,
  signer_role TEXT NOT NULL CHECK(signer_role IN ('dpo','supervisor','admin','lea_officer','operator')),
  signature_data TEXT,                    -- HMAC podpis lub HSM signature (base64)
  signed_at INTEGER NOT NULL
);
CREATE INDEX idx_claim_sigs_claim ON policy_claim_signatures(claim_id);
CREATE INDEX idx_claim_sigs_user ON policy_claim_signatures(signer_user_id);

-- ============================================================================
-- F2 — gate check cache (dla performance, bo gate_check wywoływany przed każdym service_call klasy C)
-- ============================================================================
CREATE TABLE gate_check_cache (
  gate_id TEXT NOT NULL,                  -- "d4-historical", "d4-realtime"
  scope_addon_id TEXT NOT NULL,           -- per-addon cache (np. tentavision)
  satisfied INTEGER NOT NULL,             -- 0/1
  missing_claims_json TEXT,               -- lista brakujących {type, subject, scope}
  cached_at INTEGER NOT NULL,
  invalidate_at INTEGER NOT NULL,         -- TTL — np. now+60s; invalidate przy claim_add/revoke
  PRIMARY KEY (gate_id, scope_addon_id)
);

-- ============================================================================
-- F2 — audit log rozszerzenie o risk_class + opcjonalne claim reference
-- ============================================================================
ALTER TABLE audit_log ADD COLUMN risk_class TEXT
  CHECK(risk_class IN ('A','B','C','unclassified')) DEFAULT 'unclassified';
ALTER TABLE audit_log ADD COLUMN related_claim_id TEXT;
ALTER TABLE audit_log ADD COLUMN request_id TEXT;        -- correlation z alias_calls i pickup_frame
CREATE INDEX idx_audit_risk_class ON audit_log(risk_class) WHERE risk_class IN ('B','C');
CREATE INDEX idx_audit_claim ON audit_log(related_claim_id) WHERE related_claim_id IS NOT NULL;
CREATE INDEX idx_audit_request_id ON audit_log(request_id);

-- ============================================================================
-- F1a — frame_ref tracking (dla audit ścieżki frame → service → recording)
-- ============================================================================
CREATE TABLE frame_pickup_log (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  raw_frame_ref TEXT NOT NULL,            -- frame_<uuid>
  service_id TEXT NOT NULL,               -- który service pobrał
  caller_addon_id TEXT,                   -- którego addona service_call triggered
  request_id TEXT NOT NULL,               -- correlation
  picked_up_at INTEGER NOT NULL,
  result TEXT NOT NULL CHECK(result IN ('ok','token_invalid','token_expired','frame_purged','unauthorized'))
);
CREATE INDEX idx_frame_pickup_ref ON frame_pickup_log(raw_frame_ref);
CREATE INDEX idx_frame_pickup_request ON frame_pickup_log(request_id);
CREATE INDEX idx_frame_pickup_service_ts ON frame_pickup_log(service_id, picked_up_at);
```

**Migracja istniejących TEAMS_BOT_ALIASES (jednorazowa, bez fallback):**
- F1a installer wykrywa istniejące wpisy TEAMS_BOT_ALIASES w `model_aliases`
- Tworzy wpisy `model_alias_owners` z `owner_type='addon', owner_id='teams-bot'`
- **Hard-coded list w `mod.rs:1880` jest usuwana** — od F1a teams-bot deklaruje swoje aliasy w manifest `[[alias]]` (wymaga aktualizacji teams-bot addon razem z F1a). Zgodnie z project rules "no backward-compat shims"

**Migration runner properties:**
- Idempotent — uruchamia migracje 001..N w kolejności leksykograficznej
- Atomic per migracja — `BEGIN; ... apply ...; COMMIT;` z rollback on failure
- Hash verification — wykrycie modyfikacji migracji już zastosowanej → blocker (admin musi rozwiązać)
- Partial fail handling — `status='partial'` w `addon_migrations_applied` jeśli przerwano w środku
- DDL only w migracjach — runtime SQL `sql_exec` blokuje DDL (chronione w core)

---

### 6.6 Permission model (dwukierunkowy)

Model "kto moze uzyc czyjego aliasu/modelu" wprowadzony w v0.6.0. Zastepuje
poprzednie, jednostronne podejscie (samo `owner` + `is_active`) i uzupelnia
luke w bezpieczenstwie miedzyaddonowym (cross-addon abuse alias).

#### Pojecia

- **Owner** — kto zadeklarowal/wniosl zasob:
  - Alias: addon (gdy `[[alias]]` w manifescie) lub `manual` (admin recznie utworzyl w M16).
  - Model: `system` (model wbudowany w core) lub `manual:<admin_user_id>` (admin doda recznie w M8b). **Addon nigdy nie jest ownerem modelu.**
- **Consumer** — addon ktory chce wywolac cudzy alias / uzyc cudzego modelu. Owner zawsze ma dostep do swojego — pojecie consumera dotyczy wylacznie cudzych zasobow.
- **Visibility** — polityka wystawienia zasobu:
  - Alias: `private` (tylko owner) | `restricted` (owner + jawnie wymienieni consumers) | `public` (kazdy addon ktory zadeklaruje `[[uses_alias]]` dostaje auto-grant).
  - Model: `restricted` | `public` (brak `private` — bo `private` przy modelu oznaczaloby "tylko owner = system/admin", co nie ma sensu uzywkowego; system zasob nie konsumuje sam siebie).
- **Required** — w `[[uses_alias]]` / `[[uses_model]]` flaga ktora odroznia twardy wymog od soft-feature. `required=true` ⇒ lifecycle install blokuje start addona gdy brakuje grantu; `required=false` ⇒ addon sam sobie radzi (wylacza feature, audit warning).

#### Porownanie alias vs model

| Aspekt | Alias | Model |
|--------|-------|-------|
| Mozliwy owner | `addon:<id>` lub `manual` | `system` lub `manual:<admin_id>` |
| Czy addon moze byc ownerem | tak | nie |
| Liczba stanow visibility | 3 (`private` / `restricted` / `public`) | 2 (`restricted` / `public`) |
| Czy addon moze udostepniac | tak (przez `visibility` w `[[alias]]`) | nie (model jest zasobem systemu) |
| Deklaracja konsumpcji | `[[uses_alias]]` | `[[uses_model]]` |
| Sciezka wywolania | `service_call(alias, method, payload)` | bezposredni resolver model_id (rzadkie) |

#### Reguly grantu

| Visibility | Co dzieje sie przy install consumera ktory ma `[[uses_*]]` |
|------------|-------------------------------------------------------------|
| `public` | Auto-grant: rekord w `addon_uses_alias` / `addon_uses_model` ze `status='granted'` zaraz po install. |
| `restricted` | Jesli `consumer_addon_id` jest w `allowed_consumers` ownera → auto-grant. W przeciwnym razie `status='pending'` — admin musi recznie zatwierdzic w M16b (alias) / M8b (model). |
| `private` (tylko alias) | `status='denied'` — niemozliwy do uzycia. Gdy `required=true`, install rejected. |

Resolver aliasow przy kazdym `service_call`:
1. Jesli `caller_addon_id == owner_id` → przepuszcza.
2. Inaczej sprawdza `addon_uses_alias` dla `(caller_addon_id, alias_id)` ze `status='granted'`.
3. Brak rekordu lub status != `granted` → `ABI_ERR_PERMISSION` + audit `alias_calls.result='permission_denied'`.

#### Mapowanie na DB (nowe migracje #14-#19 z F1a M1.W5)

- `model_alias_visibility(alias_id PK, visibility, created_at)` — visibility per alias (osobno od `model_alias_owners` zeby admin mogl bumpnac visibility bez zmiany ownera).
- `model_alias_consumers(alias_id, consumer_addon_id, granted_by_user_id, status, created_at)` — lista grantow per alias (PK `(alias_id, consumer_addon_id)`).
- `model_visibility(model_id PK, visibility, created_at)` — analogicznie dla modeli.
- `model_consumers(model_id, consumer_addon_id, granted_by_user_id, status, created_at)` — granty per model.
- `addon_uses_alias(addon_id, alias_id, required, reason, status, created_at)` — deklaracje z manifestu consumera. `status` mirrors `model_alias_consumers.status` po reconcile.
- `addon_uses_model(addon_id, model_id, required, reason, status, created_at)` — analogicznie dla modeli.

Szczegolowy DDL — w §6.5 (planowane do dolozenia w implementacji M1.W5 Chunk C, zob. `notes/tentavision-f1a-implementation.md`).

#### UI

Pelne mockupy w `~/.gstack/projects/Slyb00ts-TentaFlow/designs/tentavision-v1/`:

- **M8b** — Model registry: kolumna "Visibility" + dialog "Manage consumers" per model.
- **M12b** — Addon settings: zakladka "Permissions used" z lista `[[uses_alias]]` i `[[uses_model]]` plus status grantu.
- **M15b** — Install wizard, krok 4: lista wymaganych grantow (auto-granted highlighted, pending pokazane jako warning, denied/private blokuja install gdy `required=true`).
- **M16b** — Services → Aliasy: kolumna "Visibility" + dialog "Manage consumers" per alias (analogicznie do M8b).

---

## 7. Modele AI i ich rola

Dla każdej domeny: rekomendacja produkcyjna (2026 SOTA + codex review), alternatywa, CPU fallback.

### D1 — ADR

| Krok | Produkcja | Alternatywa | CPU fallback |
|------|-----------|-------------|---------------|
| Detekcja cysterny | YOLO11m (custom fine-tune transport) | RF-DETR, YOLO12 | YOLO11n |
| Detekcja tablicy ADR | YOLO11s (fine-tune ~2k zdjęć) | RT-DETR | YOLO11n |
| OCR cyfr UN + Kemler | PP-OCRv5 (PaddleOCR 3.x) lub PARSeq | TrOCR | Tesseract (słaba) |
| Klasyfikacja czytelności | ResNet50 binarny + score | EfficientNet-B0 | mniejszy ConvNet |
| Walidacja ADR | tabela ADR 2025 lokalnie (regex + lookup) | — | — |

### D2 — Anomalie zachowań

| Poddomena | Produkcja |
|-----------|-----------|
| Pose + tracking (wspólny) | YOLO11-pose + BoT-SORT |
| Upadek / zasłabnięcie | heurystyka kąty kości + lightweight temporal CNN |
| Agresja / bójka | VideoMAE V2 lub InternVideo2 fine-tuned (RWF-2000 + site) |
| Broń | YOLO11m fine-tune (WeaponS, Sohas + site) — human-in-loop |
| Wandalizm | klasyfikator akcji + change detection |

### D3 — Pozostawiony bagaż

| Krok | Produkcja |
|------|-----------|
| Detekcja bagażu | YOLO11m (COCO + ABODA + Tumult) |
| Tracking | BoT-SORT / StrongSORT z appearance embed |
| Asocjacja bagaż↔osoba | reguły geometryczne + IoU history |
| Re-id powrót | TransReID lub CLIP-ReID (OSNet jako CPU fallback) |

### D4 — Re-identyfikacja

| Komponent | Produkcja | Embed |
|-----------|-----------|-------|
| Face detect | SCRFD-10g | — |
| Face embed | **AdaFace** | 512 |
| Person detect | YOLO11m + BoT-SORT | — |
| Person re-id | **TransReID** lub **CLIP-ReID** | 512–768 |
| Gait (eksperymentalne) | GaitBase z dokumentacją ograniczeń | 256 |

### D5 — Wyszukiwanie po atrybutach

| Atrybut | Model |
|---------|-------|
| Open-vocab | **SigLIP / SigLIP2** lub **EVA-CLIP** + dedykowane attribute heads |
| Tablice | LPRNet / DTRB + walidator PL/EU |
| Marka/model/kolor auta | YOLO11 + klasyfikator (VeRi-776 + Stanford Cars) |
| Wiek/płeć | **WYŁĄCZONE** domyślnie (RODO/AI Act) |

### D6 — Generic detection

YOLO11 (n/s/m wg HW). Custom-class transfer learning. Dashboard heatmapy, liczniki, zone counts.

---

## 8. Connectory kamer

Core moduł `tentaflow-camera-ingest`. Addon nie dotyka RTSP — tylko przez Camera API.

| Vendor / Protokół | Priorytet | Notatki |
|-------------------|-----------|---------|
| **FakeFile** (mp4/mkv replay) | **F1a** | dev + acceptance tests |
| RTSP universal + HTTP snapshot fallback | **F1b** | must-have prod |
| ONVIF Profile S (live) | F1b | discovery + RTSP |
| ONVIF Profile T (advanced) | F1b | H.265, eventy |
| ONVIF Profile M (analytics metadata) | F2 | edge analytics |
| ONVIF Profile G (recording/search) | F2 | forensics |
| Hikvision ISAPI | F8 | wariancje firmware, ANPR onboard |
| Dahua CGI/DSS | F8 | wariancje |
| Axis VAPIX + ACAP | F8 | edge analytics |
| Hanwha (WiseNet) | F8 | enterprise |
| Bosch | F8 | IVA onboard |
| Avigilon / Motorola | F8 | enterprise |
| Milestone XProtect | F8 | import (VMS overlay) |
| Genetec | F8 | import |
| Frigate | F8 | OSS migracja |
| UniFi Protect | F8 | API niestabilne, pinować wersje |
| Reolink | F10 | konsumencki |
| MJPEG / HTTP push | F10 | legacy |

**Decyzja techniczna camera ingest backend:** §16.1.

**Gotchas wykrywane automatycznie:** firmware tier, region lock, ONVIF disabled, digest auth quirks, TLS cipher mismatch, admin permission.

**Recording strategy:** core ma własny ring-buffer (Recording API). Dla VMS-owych vendorów hybrid opcjonalny.

---

## 9. Dane

### 9.1 SQL schema TentaVision (ANSI subset)

W plikach `migrations/001_init.sql`, `002_*.sql`. Uruchamiane przez core przy install + version bump.

```sql
-- 001_init.sql

CREATE TABLE cameras (
  id              TEXT PRIMARY KEY,
  vendor          TEXT NOT NULL,         -- 'fake_file'|'rtsp'|'onvif'|'hikvision'|...
  url             TEXT NOT NULL,
  credentials_ref TEXT,
  location        TEXT,
  retention_class TEXT NOT NULL CHECK(retention_class IN ('A','B','C')),
  ownership       TEXT NOT NULL,
  shared_with     TEXT,                   -- JSON array
  added_at        INTEGER NOT NULL,
  last_seen       INTEGER,
  health_flags    TEXT                    -- JSON
);
CREATE INDEX idx_cameras_vendor ON cameras(vendor);

CREATE TABLE profiles (
  id           TEXT PRIMARY KEY,
  name         TEXT NOT NULL,
  flow_id      TEXT NOT NULL,             -- FK do core flows (cross-DB)
  schedule     TEXT,
  retention    TEXT,
  data_class   TEXT NOT NULL CHECK(data_class IN ('A','B','C')),
  active       INTEGER NOT NULL DEFAULT 1,
  quick_params TEXT
);

CREATE TABLE profile_cameras (
  profile_id TEXT NOT NULL,
  camera_id  TEXT NOT NULL,
  PRIMARY KEY (profile_id, camera_id)
);

CREATE TABLE alarms (
  id           TEXT PRIMARY KEY,
  ts           INTEGER NOT NULL,
  camera_id    TEXT NOT NULL,
  detector     TEXT NOT NULL,
  subtype      TEXT,
  confidence   REAL,
  status       TEXT NOT NULL CHECK(status IN ('pending','confirmed','rejected','escalated')),
  clip_ref     TEXT,
  operator_id  TEXT,
  notes        TEXT,
  confirmed_at INTEGER
);
CREATE INDEX idx_alarms_ts ON alarms(ts);
CREATE INDEX idx_alarms_camera_ts ON alarms(camera_id, ts);
CREATE INDEX idx_alarms_status ON alarms(status);

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

CREATE TABLE legal_grants (
  id            TEXT PRIMARY KEY,
  authority     TEXT NOT NULL,
  case_no       TEXT NOT NULL,
  expiry        INTEGER NOT NULL,
  scope         TEXT NOT NULL,
  dpo_signature TEXT,
  signed_by     TEXT NOT NULL,
  issued_at     INTEGER NOT NULL,
  is_active     INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX idx_grants_expiry ON legal_grants(expiry);

CREATE TABLE zones (
  id           TEXT PRIMARY KEY,
  camera_id    TEXT NOT NULL,
  name         TEXT NOT NULL,
  type         TEXT NOT NULL CHECK(type IN ('polygon','line','exclude')),
  polygon_json TEXT NOT NULL,
  color        TEXT,
  used_by_detectors TEXT
);

CREATE TABLE schedules (
  camera_id  TEXT NOT NULL,
  day_of_week INTEGER NOT NULL,
  hour_from  INTEGER NOT NULL,
  hour_to    INTEGER NOT NULL,
  profile_id TEXT NOT NULL,
  PRIMARY KEY (camera_id, day_of_week, hour_from)
);

CREATE TABLE rules (
  id         TEXT PRIMARY KEY,
  name       TEXT NOT NULL,
  expression TEXT NOT NULL,
  action     TEXT NOT NULL,
  active     INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE evidence_exports (
  id                 TEXT PRIMARY KEY,
  case_no            TEXT NOT NULL,
  authority          TEXT NOT NULL,
  legal_grant_id     TEXT,
  signed_package_id  TEXT NOT NULL,
  chain_hash         TEXT NOT NULL,
  created_at         INTEGER NOT NULL,
  created_by         TEXT NOT NULL
);

CREATE TABLE search_audit (
  id               TEXT PRIMARY KEY,
  user_id          TEXT NOT NULL,
  query_type       TEXT NOT NULL,
  query_hash       TEXT NOT NULL,
  time_window_from INTEGER,
  time_window_to   INTEGER,
  hits_count       INTEGER,
  legal_grant_id   TEXT,
  risk_class       TEXT CHECK(risk_class IN ('A','B','C')),
  ts               INTEGER NOT NULL
);
CREATE INDEX idx_search_audit_ts ON search_audit(ts);
```

### 9.2 Vector namespaces

| Namespace | Wymiary | Distance | Klasa | Gate | Indeksuje |
|-----------|---------|----------|-------|------|-----------|
| attributes | 768 | cosine | B | — | SigLIP2 embedding kadrów osób |
| plates | 256 | cosine | B | — | LPRNet/embedding tablic rejestracyjnych |
| faces | 512 | cosine | C | d4-historical | AdaFace embedding twarzy |
| persons | 512 | cosine | C | d4-historical | TransReID embedding sylwetek |

### 9.3 Recording

| Aspekt | Specyfikacja |
|--------|--------------|
| Format | MP4 H.264/H.265, segmenty 5-min |
| Ring-buffer | Per kamera, w core. Limit: konfigurowalny (default 4 TB total). Retention z `cameras.retention_class` |
| Save segment | `recording_save_segment(cam, start, end)` → ClipRef. Segment "wykuty" — nie zostanie purged przed retention |
| Snapshot | PNG, `recording_save_snapshot(cam, ts)` |
| Access | Signed URLs `recording_get_url(ref, ttl)` |
| Purge | Core background co 1h, respektuje retention per ClipRef |

---

## 10. RODO / EU AI Act / claims-based gates

### 10.1 Klasyfikacja detektorów

| Klasa | Detektory | RODO/AI Act |
|-------|-----------|-------------|
| **A** niskie | D1, D3 (obiekt), D6 generic | RODO art. 6.1.f + signage; AI Act poza Annex III |
| **B** średnie | D2 (sylwetka), D3 (z asocjacją), D5 atrybuty | DPIA; signage; krótka retencja |
| **C** wysokie | D4 face/person/gait, D5 wiek/płeć | **AI Act Annex III high-risk**. Real-time w publicznej przestrzeni **Art. 5** zakazane poza wąskimi wyjątkami |

### 10.2 EU AI Act timeline (zaktualizowany po porozumieniu KE 7 maja 2026)

| Data | Co | Dotyczy TentaVision |
|------|----|---------------------|
| **2.02.2025** | Prohibitions (Art. 5) + AI literacy | D4 real-time w publicznej przestrzeni objęte Art. 5 |
| **2.08.2025** | GPAI obligations (general-purpose AI models) | Modele typu SigLIP/AdaFace mogą być klasyfikowane jako GPAI |
| **2.08.2026** | **Transparency obligations** (Art. 50) — informowanie użytkowników że wchodzą w interakcję z AI, watermarking deep fakes itd. Nie pełen Annex III. | Klauzula informacyjna obowiązkowa (znaki "monitoring + AI") |
| **2.12.2027** | **Annex III high-risk obligations — biometric identification, w tym remote biometric ID** (po porozumieniu Rady i Parlamentu z 7.05.2026) | **D4 (face + person re-id) — pełne wymogi** od tej daty |
| **2.08.2028** | Embedded products + safety components | Dotyczy gdy TentaVision jest częścią safety-critical system (np. lotnisko) |

**Źródła oficjalne:**
- AI Act 2024/1689
- European Commission, "EU agrees to simplify AI rules" (porozumienie 7.05.2026)
- Council of the EU press release 7.05.2026

**Implikacje dla TentaVision:**
- F1a–F5: nie dotykamy D4 produkcyjnie. Klauzula informacyjna od F2 (do M10 generator)
- F6: implementujemy claims engine z myślą o 2.12.2027 deadline
- F7: D4 produkcja gotowa **przed** 2.12.2027 dla deploymentów które chcą go używać przed terminem (LEA z legal grant)
- Embedded products (lotnisko, dworzec) — zaprojektowane z myślą o 2.08.2028

### 10.3 Twarde mechanizmy

1. **Profil prawny przy onboardingu (M13/M15):** Komercja / Transport / Lotnisko / Służby. Determinuje dostępność D4
2. **Gate `d4-realtime` / `d4-historical`:** addon w manifest deklaruje `[[gate]] required_claims`. Core policy engine sprawdza claims przed wywołaniem aliasu klasy C
3. **Aliasy klasy C tworzone z `is_active=0`:** router odmawia dopóki claims niezaspokojone
4. **Retencja per klasa:** A:30 / B:14 / C:7 dni (override tylko z uzasadnieniem prawnym → audit)
5. **Maskowanie twarzy:** core camera-ingest blur-uje w klatkach do live view dla I linii. Unmask wymaga claim
6. **Right to be forgotten:** narzędzie w M10 → `vector_delete("faces", [subject_id])`
7. **Audit hash-chain:** query/eksporty klasy B/C → append-only + WORM
8. **Generator dokumentów:** DPIA, FRIA, klauzul info, znaków monitoring+AI

### 10.4 Profil "Służby" nie jest magiczną rolą

Profil `lea` daje **dostępność** D4, ale każdy real-time/eksport wymaga aktywnego `LegalGrant`:
- authority (Policja / Prokuratura / ABW / SG)
- case_no (sygnatura)
- expiry
- DPO sign + supervisor sign
- automatyczne powiadomienie DPO

### 10.5 Wbudowane referencje (assets/legal/)

- EU AI Act 2024/1689 (art. 5, art. 11, Annex III, Annex IV)
- EDPB Guidelines 3/2019 (video processing)
- RODO art. 6, 9, 35
- Ustawa o ochronie osób i mienia (PL)
- Ustawa o Policji (art. 20)
- KPK art. 217 (zabezpieczenie dowodów)
- ADR 2025 tabela
- Szablony DPIA, FRIA, klauzul

---

## 11. Bezpieczeństwo

| Aspekt | Mechanizm |
|--------|-----------|
| Kamery — TLS | RTSPS preferowane (camera-ingest module) |
| Poświadczenia | Secret vault TentaFlow + rotacja 90d |
| SSRF | Camera connector w core sprawdza allowlist; addon nie dotyka RTSP |
| Segmentacja | Kamery VLAN, runtime osobny, dashboard osobny |
| Audit tamper-resistant | Append-only + Merkle chain + WORM (S3 immutable lub `chattr +a`) |
| mTLS node-to-node | Już w TentaFlow |
| Role | viewer / operator / analyst / dpo / admin / lea-officer |
| HSM | Yubikey HSM2 (prod) / SoftHSM (dev) |
| TSA | RFC 3161 (freetsa.org / digistamp) |
| Anchoring | F10: BTC (OpenTimestamps) daily |
| FrameRef security | scoped token, TTL 30s, replay protection, node locality (§6.4) |
| UI components signature | Ed25519 podpis bundle JS, weryfikacja przy install, iframe sandbox dla high-risk |
| Cross-addon FrameRef | Nie — scope_token per addon, audit anomalii |

---

## 12. Wydajność i SLO

| Metryka | Target |
|---------|--------|
| Latencja D2 broń/agresja → alarm | < 1.5 s p95 |
| Latencja D1/D3/D6 → alarm | < 3 s p95 |
| FPS per kamera | 5–15 dla D1/D3/D6; 15–25 dla D2 |
| Kamery na 1× RTX 4070 (mixed) | ~8 mixed / ~16 light |
| GPU utilization | 60–80% |
| Wyszukiwanie D5 (10M klatek) | < 800 ms p95 |
| Re-id query D4 (100k embed) | < 200 ms p95 |
| `service_call` overhead | < 5 ms poza inference |
| `stream_next` poll | < 1 ms gdy bufor niepusty |
| Audit append | < 10 ms (write-ahead) |
| Pickup_frame (service-to-core) | < 20 ms |
| FrameRef token issuance | < 1 ms |

**Bench CLI:** `tentavision bench --cameras N --profile mixed`.

**Heavy combo D2+D4+D5 łamie mid tier** bez time-slicing — UI ostrzega "overprovisioned".

---

## 13. Dataset strategy

- **Zbieranie:** per deployment right-to-collect bucket (opt-in + DPIA). Sample per kamera per detektor
- **Labeling:** wbudowane narzędzie w M5 "label this alarm" + integracja Label Studio offline. Stratifikacja per site
- **Negative examples:** hard-negatives z FP alarmów (operator "fałszywy" → training set)
- **Drift detection:** monthly job — rozkład embeddingów vs baseline; alarm DPO przy drift > threshold
- **Per-site calibration:** thresholdy + adaptacja per godzina (rano/popołudnie/noc)
- **Retraining:** offline, nie blokuje produkcji; nowy model → A/B shadow → switch z rollback

---

## 14. Evaluation harness

Wbudowany w runtime, auto + on-demand:

- **Per-domain P/R/F1** na walidacyjnym secie deployment-specific
- **FP per hour per camera** (alert fatigue — krytyczna)
- **Subgroup metrics** (RODO fairness): płeć, wiek, oświetlenie, pora dnia
- **Latency histograms** per operator (p50/p95/p99)
- **GPU utilization** per model
- **AI Act post-market monitoring:** raport miesięczny Annex IV — do DPO

**CLI:** `tentavision eval --profile <id> --period 7d`.

---

## 15. Roadmap implementacyjny F0–F10

| Faza | Zakres | Kryterium zamknięcia |
|------|--------|----------------------|
| **F0** — Plan + research | Plan v0.5 (ten dok), research SDK, mockupy M1–M16 | Akceptacja użytkownika |
| **F1a** — Foundation (minimum SDK + thin addon) | API-1 (service_call z method), API-1a/b/c (aliasy + UI M16), API-2 (storage manifest), API-3 (SQL host fn SQLite), API-4 (migrations), API-4b (per-addon FS), API-5 z **FakeFile camera** (mp4 replay), API-6 (Streaming + FrameRef), API-8 basic (snapshot/segment/get_url), Test matrix §17 | TentaVision MVP: 1 FakeFile camera, alias yolo działa, snapshot zapisany, alarm w SQL, view w M1+M14, M16 działa, e2e test green |
| **F1b** — Real cameras | RTSP universal + ONVIF Profile S/T connectors w core camera-ingest. Wszystko inne z F1a działa | 1 prawdziwa kamera RTSP w MVP |
| **F1c** — Custom UI components | API-10 z signature + iframe sandbox. Components: tv-video-grid, tv-zone-editor, tv-heatmap, tv-results-grid | M2 live grid z bbox overlay działa; M9 zone editor; M1 heatmap; M6 results grid |
| **F2** — D1 end-to-end + Policy basic | Manifest TentaVision finalny, addon WASM szkielet, D1 ADR Flow (4 bloki), services yolo+ocr w Dockerze, M3+M4+M5 ekrany, API-15a basic claims store (`gate_check` for unmask + retention override), API-13 risk_class w audit, API-11 flow_invoke, API-12 flow templates, API-7 Vector | 1 prawdziwa kamera → ADR check → alarm w M5; podstawowe gates działają |
| **F3** — D3 luggage + Profile + Strefy + Recording full | M9 strefy (z F1c components), D3 luggage Flow + service, API-8 full (ring-buffer z retention policies) | 4+ kamery z 3 profilami; D3 wykrywa luggage |
| **F4** — Search D5 + VLM | M6 wyszukiwarka działająca, services siglip2+lprnet, indeksowanie atrybutów | Search po atrybutach na 24h nagrań |
| **F5** — D2 anomalie | Services videomae-v2 + weapons, M5 z workflow potwierdzania | D2 z site-calibrated FP <5% |
| **F6** — Legal hard gates full + Audit + Evidence | API-15 full (claims workflow z DPO+supervisor sign), M7 D4 gate, M10 audit+RODO full, M11 evidence + HSM/TSA (API-9), M13 profil prawny full | Komercja blokuje D4, Służby z claim pozwala z audit do WORM |
| **F7** — D4 produkcja | Services adaface + transreid, vector faces/persons, D4 query tylko z aktywnym LegalGrant, post-market monitoring | Re-id real tylko z grantem |
| **F8** — Vendor connectors enterprise + PostgreSQL | Hikvision, Dahua, Axis ACAP, Hanwha, Bosch, UniFi P2, Milestone import. PostgreSQL backend (per-addon DB + role). Model rollback. ONNX upload. Verifier CLI | 4+ vendory; opcja PG dla dużych instalacji |
| **F9** — Flow templates polished + Generic install wizard | API-12 polished, generic wizard działa dla TentaVision + min 1 innego addona | Generic wizard production-ready |
| **F10** — Scale & Edge | TensorRT/OpenVINO/Jetson edge, multi-node load balance, model rollback <60s, advanced eval, BTC anchoring | Jetson POC + 2-node cluster |

**Łączny czas estymowany F0–F2:** 4–8 miesięcy zespołowych (zależnie od liczebności).

**F1a sam estymata (revised po codex review):** ~10–16 tygodni jednego seniora **lub** ~6–8 tygodni 2-osobowego zespołu. Zakres F1a obejmuje kilka równoczesnych subsystemów core: ABI extensions, alias management + UI M16, SQL host functions + per-addon SQLite + migrations runner, per-addon FS sandbox, FakeFile camera connector, streaming + RawFrameRef + PickupToken, basic recording (snapshot/segment/url), plus pełna bateria security/perf/e2e tests z §17. Pierwotna estymata 6-10 tygodni 1 dev była optymistyczna.

**Sugerowany F1a milestone split:**
- Tygodnie 1-3: SDK extensions (manifest parser dla nowych sekcji, ABI scaffolding, error codes, versioning)
- Tygodnie 4-6: SQL host functions + migrations runner + per-addon FS sandbox + alias management ABI
- Tygodnie 7-9: Streaming + RawFrameRef + PickupToken + FakeFile camera + basic recording
- Tygodnie 10-12: M16 UI basic + integration tests + security tests (§17.5)
- Tygodnie 13-16: UI e2e tests M1+M14+M15, performance benchmarks (§17.8), bug fixes, F1a acceptance

---

## 16. Decyzje techniczne

### 16.1 Camera ingest backend — GStreamer

**Decyzja:** Core camera-ingest module używa **GStreamer** jako bibliotekę bazową.

**Uzasadnienie:**
- Wsparcie multi-codec out of box (H.264/H.265/MJPEG/AV1)
- Hardware acceleration (NVDEC/VAAPI/Intel QSV)
- RTSP/ONVIF/RTMP/HLS/SRT bez własnej implementacji
- Vendor plugin ecosystem (Hikvision quirks, Axis ACAP retrieval)
- Stabilność i licencja LGPL — komercyjne OK

**Alternatywy odrzucone:**
- **FFmpeg as library (libav)** — niższa abstrakcja, więcej własnej pracy
- **OpenCV** — wysoka abstrakcja ale brak ONVIF / vendor quirks
- **Własna implementacja RTSP** — zbyt drogie, walka z każdym vendor quirk

**Implementacja:** `tentaflow-core/src/camera_ingest/gstreamer_session.rs` — per-kamera GStreamer pipeline trzymany w thread pool, frame_refs emitowane przez bounded channel do streaming bus.

### 16.2 model_aliases vs service_aliases — używamy model_aliases

W TentaFlow istnieją dwie tabele:
- `service_aliases` (`migrations.rs:201`) — alias → service_id (1:1, prostsze)
- `model_aliases` (`migrations.rs:225`) — alias → target_model + fallbacks + strategy

**Decyzja:** TentaVision używa **`model_aliases`** dla wszystkich 6 aliasów AI (yolo, ocr, action, vlm, face-embed, reid). Powód: fallback chain jest kluczowy (jeśli node-gpu-A padnie, router musi spaść na node-gpu-B).

**`service_aliases` pozostaje** dla prostszych przypadków (np. teams-bot-sidecar — jeden konkretny service, brak fallback). Nie usuwamy.

### 16.3 Permission naming — `secrets.*`, `events.*`, kropka separator

Sprawdzone w kodzie:
- Teams-bot manifest używa `secrets.read`, `secrets.write`, `events.publish`, `events.subscribe`, `ui.render`
- Host functions w core używają tych samych nazw

**Decyzja:** TentaVision używa identyczne nazewnictwo. Plan v0.4 miał `secret.read` (bez "s") — to był błąd.

### 16.4 FrameRef lifecycle

- **Issuance:** core przy `stream_next` zwraca frame_ref z embedded uuid + node_id (nie scoped token jeszcze)
- **Scoping:** dopiero `service_call(alias, method, payload{frame_ref})` powoduje wystawienie scoped token (HMAC core master key, payload `{frame_ref, service_id, request_id, expiry}`) który jest przekazany do service w QUIC payload
- **TTL:** 30s default. Configurable per node `[stream] frame_ref_ttl_ms`
- **Cleanup:** stream_close → invalidate wszystkie aktywne refs ze stream. Background job co 60s purge expired
- **Memory:** LRU shared memory, default 1024 ramki per node
- **Replay:** scope_token jest one-shot — pickup_frame invalidate

### 16.5 SQL backend default w F1a — SQLite, kropka

PostgreSQL przesunięte do F8. Powody:
- F1a chce być **uruchamialne lokalnie** (developer, fake camera, fast iteration)
- PG wymaga external runtime, admin permissions, role management — to F8 enterprise concern
- SQLite per-addon plik wystarcza dla deploymentów < 100 kamer / < 10M alarmów (estymata)
- Migracja SQLite → PG jako tool w M14 (button "Migrate") implementowany w F8

### 16.6 Strategy aliasów w MVP — tylko `first_available`

`round_robin` w F2 (M16 inline editor już to obsługuje), `weighted` wycofane (over-engineering w MVP).

### 16.7 Custom UI components — Ed25519 + iframe sandbox

- Manifest deklaruje `signature` (puste / placeholder w dev, real podpis w prod packaging)
- Tools packaging: `tentavision pack --sign <ed25519_key>` generuje signed bundle
- Core przy install weryfikuje podpis przeciwko allowed signers (TentaFlow corp signer + opcjonalnie user-added signers)
- Renderowanie: `risk=low` → shadow DOM z CSP; `risk=high` → iframe `sandbox="allow-scripts"` + postMessage bridge z enumerated API

---

## 17. Test matrix dla F1a

Acceptance criteria F1a — musi zielono przed merge:

### 17.1 Unit tests (Rust)

| Komponent | Co testuje |
|-----------|-----------|
| `service_call` dispatcher | Routowanie alias → target wg strategy, fallback chain, `executed_by` w response |
| Alias CRUD repository | create_or_reactivate, deactivate, get z fallback, list_by_owner |
| FrameRef token issuer | issue → verify → invalidate, TTL expiry, replay protection |
| FakeFile camera connector | mp4 replay, frame timing, last_seen reporting |
| Streaming bus | subscribe, next z timeout, close, backpressure (Drop messages) |
| SQL host functions | exec, query, transaction, parameterized queries, errors |
| Per-addon FS sandbox | path resolution, isolation (addon A nie widzi addon B) |
| Manifest parser | nowe sekcje (`[storage]`, `[[alias]]`, `[[gate]]`, `[[vector_namespace]]`, `[[flow_template]]`, `[[ui_component]]`) |
| Migrations runner | apply 001..N w kolejności, idempotent, rollback on failure |

### 17.2 Integration tests

| Scenariusz | Setup | Asercja |
|------------|-------|---------|
| End-to-end fake camera → alias → SQL | 1 FakeFileCamera (sample_traffic.mp4), zarejestrowany fake-yolo service, TentaVision installed z manifestem | Po 60s replay: 1+ alarm w `alarms` SQL, M5 feed pokazuje, audit log ma wpisy |
| Alias fallback | yolo11m down, yolo11s up | `service_call` zwraca `executed_by=yolo11s`, `fallback_used=true` |
| Alias gate | tentavision-face-embed alias istnieje, claims puste | `service_call("tentavision-face-embed",...)` zwraca `ABI_ERR_NOT_FOUND` (alias inactive) |
| Stream backpressure | Addon nie wywołuje `stream_next` przez 10s | Po wznowieniu — `StreamMessage::Drop{count}` w buforze |
| SQL injection guard | `sql_query("SELECT ... WHERE x = ?")` z user-controlled param | Parametr bind safe, brak SQL injection |
| FrameRef leak | Addon próbuje przekazać frame_ref do innego service-a niż request | Core odmawia, audit anomaly |
| Migration apply | Fresh addon install | `migrations/001..N` zastosowane, tabele istnieją |
| Recording snapshot | `recording_save_snapshot(cam, ts)` → `recording_get_url(snapshot_ref, ttl=10)` | URL działa przez 10s, potem 403 |
| M16 admin flow | Admin tworzy manual alias "test-alias" → ustawia target | INSERT w `model_aliases` + `model_alias_owners` z owner_type=manual |

### 17.3 WASM ABI tests

| Test | Co |
|------|----|
| service_call malformed input | `payload` nie-JSON → ABI_ERR_OPERATION |
| sql_exec DDL | `CREATE TABLE ...` z runtime → ABI_ERR_PERMISSION |
| alias_create z reserved id | `id="system-*"` → ABI_ERR_OPERATION |
| Permission denied | `service.call` not granted → ABI_ERR_PERMISSION + audit |

### 17.4 UI API tests (web frontend)

| Test | Setup | Asercja |
|------|-------|---------|
| M16 lista aliasów | 6 aliasów TentaVision + 3 manual + 5 teams-bot zarejestrowanych | Tabela pokazuje 14 wierszy z poprawnymi ownerami |
| M16 edit primary | Klik edit → zmiana target → save | `UPDATE model_aliases` + wpis `model_alias_changes` |
| M16 fallback reorder | Drag-to-reorder fallback list | JSON array w `fallback_targets` zmienia kolejność |
| M14 readonly view | TentaVision installed | Pokazuje 6 aliasów z current targets z `model_aliases` |
| M15 install wizard | TentaVision package z manifest | 6 kroków, krok 3 pokazuje 6 aliasów do utworzenia |

### 17.5 Security-focused tests (krytyczne dla F1a gate)

| Test | Co testuje |
|------|-----------|
| Pickup token replay | Service używa `pickup_token` raz → 200 OK. Drugi pickup z tym samym tokenem → 403 + audit anomaly |
| Pickup token TTL expiry | Service odczekuje 31s → 410 Gone + audit |
| Pickup token cross-service | Service A próbuje użyć tokenu wystawionego dla service B → 403 + audit anomaly |
| Pickup token forge | Service wysyła HMAC token podpisany własnym kluczem (nie core) → 403 |
| Frame URL signing | Browser request `/frame/<ref>?token=...` z modyfikowanym tokenem → 403 |
| Frame URL TTL | URL przez 600s nie powinien działać po expiry → 403 |
| Path traversal FakeFile camera | `camera_add({url:"../../../etc/passwd"})` → ABI_ERR_OPERATION + audit. FS sandbox blokuje |
| Path traversal FS sandbox addon | Addon próbuje `sql_query` z attached database `'/etc/passwd'` → blocked (DDL restriction) |
| Per-addon FS isolation | Addon A próbuje czytać `/home/critix/.tentaflow/addons/addon-b/data.db` → blocked przez FS sandbox |
| SQL injection | `sql_query("SELECT * FROM users WHERE name = '${user_input}'")` z user_input zawierającym `'; DROP TABLE` → params bind, brak injection |
| Quota — KV | Addon próbuje zapisać 10001 kluczy → `ABI_ERR_QUOTA_EXCEEDED` |
| Quota — Vector | Addon upsert 1001 wektorów w jednym call → `ABI_ERR_PAYLOAD_TOO_LARGE` |
| Quota — SQL | Query timeout 31s → `ABI_ERR_TIMEOUT` + alert |
| Quota — stream buffer | Addon nie wywołuje stream_next przez 60s, buffer 100 zapełnia się → core dropuje najstarsze + emit `Drop{count}` na resume |
| DoS — service_call flood | Addon woła `service_call` 10000× w sekundę → rate limit `service.call` per addon = 1000/min |
| DoS — recording_save_segment | Addon żąda 1000 segmentów naraz → queue z fair-share scheduler |
| Manifest permission edge cases | Permission z gate niezaspokojonym → `permission_get_status` zwraca `gate_denied` mimo accept przez admin |
| Migration on partial fail | Apply 001..003. 002 fails w środku → `addon_migrations_applied` zapisuje partial; restart próbuje od 002 z czystym DB state |
| Migration hash modify | Admin modyfikuje 001.sql po apply, addon upgrade → hash mismatch → migration runner odmawia, audit + alert |
| Migration on existing DB | Addon reinstall (gdy DB zostaje z poprzedniej instalacji) → wykrywa już applied migracje, idempotent |
| FrameRef leak detection | Addon w SQL trzyma `alarms.frame_ref` z tygodnia temu, próbuje `service_call` z tym ref → `ABI_ERR_FRAME_PURGED` (frame już LRU-evicted) |
| Cross-addon FrameRef | Addon A dostaje `raw_ref` z stream. Próbuje przekazać addon B przez `event_publish` → addon B nie może go użyć w `service_call` (scope mismatch) |
| Service registration auth | Niezarejestrowany service próbuje `pickup_frame` → 403 |
| Recording purge race | `recording_get_url(clip_ref, ttl=600)` → core wykonuje purge między signing a fetch → URL valid ale 404 przy fetch (correct behavior) |
| Audit chain tamper | Manualna modyfikacja wpisu w `audit_log` → `audit_verify(from_hash)` wykrywa break + alert |
| Claim revocation propagation | DPO revoke LegalGrant w trakcie aktywnej sesji query D4 → następny `gate_check` zwraca not satisfied, alias staje `is_active=0` |
| Gate cache invalidation | `claim_add/revoke` → `gate_check_cache` purged → następne sprawdzenie czyste |

### 17.6 UI e2e tests (mockupy → frontend)

| Test | Co |
|------|----|
| M1 Dashboard render | TentaVision installed, 5 alarmów w SQL, 22 kamer → KPI tiles pokazują 22/24 kamer, 5 alarmów, heatmapa rysuje |
| M2 Live view subscription | Mock fake camera streamuje 5 fps → grid kafelków refresh co 200ms, bbox overlay renderuje |
| M3 wizard krok 1 | Klik "+Dodaj kamerę" → ONVIF discovery zwraca 3 fake kamery → lista renderuje |
| M5 alarm card | Klik alarmu → `recording_get_url(clip_ref)` → `<video>` ładuje, timeline 10 klatek renderuje |
| M6 search query | Wpisz "czerwona czapka", click Search → `service_call("tentavision-vlm", "embed")` → `vector_search` → grid wyników renderuje |
| M7 gate blocked | Profil deployment="commercial", brak LegalGrant → ekran pokazuje "Moduł Re-ID zablokowany" + checklist 6 warunków |
| M11 evidence sign | Klik "Nowa paczka" → wypełnij case_no → `evidence_sign` → URL signed package przychodzi w response |
| M14 readonly aliases | TentaVision installed → 6 aliasów w tabeli, current target widoczny, link do M16 działa |
| M15 install wizard | TentaVision package w marketplace → klik install → 6 kroków, krok 3 deklaracja aliasów → po finalize `model_aliases` ma 6 wpisów + `model_alias_owners` z owner=tentavision |
| M16 admin edit | Admin loguje, otwiera /services/aliases → tabela 21 aliasów, klik edit tentavision-yolo → zmiana primary target → save → `model_aliases.target_model` updated + `model_alias_changes` entry |

### 17.7 Fake camera dataset

`assets/test/sample_traffic.mp4` — 5-minutowy klip MP4 z ruchem (ciężarówka, samochody, osoby chodzące), używany w F1a integration tests. Plus `sample_adr_plate.mp4` — kadrowanie ciężarówki z czytelną tablicą ADR (UN 1203) do D1 acceptance. Plus `sample_corrupted.mp4` — uszkodzony plik do testów error handling.

### 17.8 Performance benchmarks (acceptance)

| Metryka | Target F1a | Test |
|---------|-----------|------|
| `service_call` overhead (bez inference) | < 5 ms p99 | mock service zwraca pusty payload od razu, mierzymy round-trip |
| `stream_next` poll latency | < 1 ms p99 (gdy buffer niepusty) | bench wsadowy 10000 calls |
| `sql_exec` simple INSERT | < 5 ms p99 | INSERT INTO alarms ... |
| `recording_save_snapshot` | < 50 ms p99 | snapshot PNG z fake camera |
| Pickup_frame round-trip | < 20 ms p99 | service request → core response z bajtami |
| FrameRef token issuance | < 1 ms p99 | bench wsadowy HMAC operations |
| Migration apply | < 2 s dla schema TentaVision (10 tabel + 8 indeksów) | full reinstall |
| Manifest parse + validation | < 100 ms | parse manifest.toml + walidacja `[[alias]]`, `[[permission]]`, etc. |

---

## 18. Otwarte pytania (zredukowane)

Po review i decyzjach §16 — pozostały:

1. **Cross-addon FrameRef sharing** — czy w F2/F3 dopuścić scoped sharing FrameRef między addonami (np. TentaVision share z AccessControl-addon)? Dziś hard no — każdy addon ma własny scope. **Decision needed przy F2.**
2. **Mesh sync model_aliases** — w multi-node TentaFlow czy `model_aliases` synchronizować przez CRDT (jak `crdt_store.rs:175`) czy single-source w master node? **Decision: CRDT sync (istniejący mechanizm). Doprecyzować w F1b dokumencie design.**
3. **`evidence_recipients` per addon vs global** — w F3 admin TentaFlow zarządza listą organów. Czy TentaVision-specific (osobna tabela per addon) czy global TentaFlow registry? **Skłaniam się do global TentaFlow — inne addony też mogą używać.**
4. **Audit WORM externalizacja format** — JSONL plus index, czy SQLite snapshot, czy CBOR? **Decision in F2.**
5. **Right to be forgotten — granularność** — usuwamy subject_id ze wszystkich namespaces na raz? Po jednym? Co z alarms.confidence które się zmienią (alarm referencyjny zostaje, embedding znika)? **Decision in F6.**

---

## 19. Glossary

| Termin | Definicja |
|--------|-----------|
| **Addon** | Pakiet WASM + manifest + assets zainstalowany w TentaFlow |
| **Alias** | Wpis w globalnej `model_aliases`: `(alias, target_model, fallback_targets, strategy, is_active)`. Addon woła `service_call(alias)`, router rozwiązuje |
| **Application** (tryb) | Addon z UI w shellu TentaFlow |
| **ClipRef** | Opaque uchwyt do segmentu nagrania w core recording manager |
| **Capabilities** | Cechy serwisu (akceptowane wejście, output, GPU requirements). Filter Flow w M4 |
| **Claim** | Zapis w policy engine: approval / grant / deployment_profile |
| **D1..D6** | Sześć domen TentaVision |
| **DPIA** | Data Protection Impact Assessment (RODO art. 35) |
| **F1a/F1b/F1c** | Pod-fazy F1 — kolejno: minimum SDK + fake camera, real cameras, custom UI components |
| **FakeFile camera** | Connector dev który replay-uje mp4 z dysku jako kamerę. Używany w F1a + acceptance tests |
| **FrameRef** | Opaque uchwyt do ramki wideo. Addon przekazuje do `service_call`, core wystawia scoped token dla service |
| **FRIA** | Fundamental Rights Impact Assessment (AI Act art. 27) |
| **Flow** | Pipeline DAG w FlowBuilder TentaFlow |
| **Flow block** | Element DAG-a Flow. TentaVision rejestruje `addon.tentavision.*` |
| **Gate** | Polityka wymagająca claims do uruchomienia operacji klasy C |
| **GStreamer** | Wybrana biblioteka camera ingest (§16.1) |
| **HSM** | Hardware Security Module — Yubikey HSM2 (prod) / SoftHSM (dev) |
| **LegalGrant** | Claim typu grant z authority + case_no + expiry |
| **Manifest** | `manifest.toml` — pełna deklaracja addona |
| **Mockup M1..M16** | 16 ekranów UI |
| **Owner** (aliasu) | `addon:<id>` lub `manual` (kolumna w `model_alias_owners`) |
| **pickup_frame** | Service-to-Core API endpoint (§6.3) — service pobiera bajty ramki po FrameRef |
| **Scope token** | HMAC token wystawiany przez core przy `service_call`, jednorazowy per service, TTL 30s |
| **Service** | Zarejestrowany Docker service z modelem AI |
| **service_call** | Host function — `(alias, method, payload) → ServiceResponse{payload, executed_by, duration_ms, fallback_used}` |
| **Strategy** | Algorytm router'a: `first_available` (F1a) / `round_robin` (F2) |
| **Suggested default** | Pole w `[[alias]]` — sugesia addona dla target_model. Admin może zmienić w M16 |
| **TentaFlow** | Główna platforma — host dla addonów |
| **TentaVision** | Ten addon — analiza obrazu z kamer |
| **TSA** | Time Stamping Authority (RFC 3161) |
| **WORM** | Write Once Read Many — bucket immutable do externalizacji audit |
