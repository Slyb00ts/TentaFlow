# Projekty (Project Studio) — przewodnik realizacji

Dokument dla zespołu/agenta wykonawczego. Opisuje JAK zrealizować moduł „Projekty" w TentaFlow tak, aby mockupy z tego katalogu stały się działającym produktem. Wszystkie fakty o platformie zweryfikowane w kodzie repo (stan 2026-07-23) — ścieżki podane. Specyfikacja funkcjonalna (zakres, role, przepływy, decyzje) jest w osobnym dokumencie „project-studio-spec-funkcjonalna" (v4); ten plik skupia się na realizacji technicznej i mapowaniu ekranów na kod.

Zasada nadrzędna: **naśladuj wzorzec ML Studio i Benchmark Studio** — to dwa istniejące, natywne moduły „studio" w core. Nie wymyślaj nowych mechanizmów tam, gdzie one już rozwiązały ten sam problem.

---

## 1. Architektura modułu (wzorzec potwierdzony w kodzie)

- **Natywny moduł core**, nie addon WASM. Wzorzec: `tentaflow-core/src/ml_studio/mod.rs` („ML Studio is a native core module … owns a SEPARATE SQLite database"). Utwórz `tentaflow-core/src/project_studio/` z: `mod.rs` (init/pool), `db.rs` (migracje), `repository.rs` (CRUD), `models.rs` (typy), plus moduły domenowe (ingest, generation, runs, git_source, environments, reports).
- **Protokół binarny** (dashboard NIGDY nie używa REST — Tier 1). Wzorzec: `tentaflow-protocol/src/benchmark.rs` + `MessageBody::BenchmarkBody(BenchmarkPayload)`. Utwórz `tentaflow-protocol/src/project_studio.rs` z enumem `ProjectStudioPayload` (pary Request/Response) i JEDNYM wariantem `MessageBody::ProjectStudioBody`. UWAGA (potwierdzone `tentaflow-protocol/src/message_body.rs:7215`): enum `MessageBody` jest na granicy 256 wariantów CBOR — dlatego wariant MUSI być pojedynczy z pod-enumem, a nowe warianty dodaje się ZAWSZE na końcu (ciborium indeksuje po pozycji). Zmiana łamiąca = podbicie `SCHEMA_VERSION` (`tentaflow-protocol/src/envelope.rs:159`, obecnie 21).
- **Trzy miejsca frontowe na każdy request** (nie ma generycznego enkodera po nazwie): (a) wasm-bindgen encoder w `tentaflow-protocol-wasm/src/lib.rs`, (b) helper w `tentaflow-core/www/js/protocol/codec.js`, (c) wywołanie `ApiBinary.one/list/action(...)` w module UI.
- **Handlery**: `tentaflow-core/src/dispatch/project_studio.rs`, makro `#[handler(variant=...)]` + `#[policy(UserSession)]` + `#[observed]` (wzorzec `dispatch/benchmark.rs`, `dispatch/ml_studio.rs`).
- **UI**: klasyczny moduł JS dashboardu (nie panel addonowy ui_v1). Utwórz `www/js/modules/project-studio.js` + `www/css/project-studio.css`, rejestracja `Router.register('projekty', …)` w `www/js/app.js`, kafelek w `www/js/modules/apps-home.js` **bez** `requiresPowerUser`, wpis nawigacji, sekcja i18n `project_studio.*` w `www/i18n/{pl,en,fr,es,de}.json`. Mockupy pokazują docelowy wygląd 1:1 (te same komponenty tf-*).

## 2. Składowanie per projekt (decyzja produktowa — potwierdzona wykonalność)

Układ (patrz spec §3.1):
```
<data>/projects.db                     # rejestr: projects, project_members, notifications, project_chats, creator_grants
<data>/projects/<project_id>/project.db  # WSZYSTKIE dane domenowe projektu
<data>/projects/<project_id>/vectors/    # kolekcje zvec projektu
<data>/projects/<project_id>/files/      # pliki źródeł + załączniki (content-addressed)
<data>/projects/<project_id>/runs/<run_id>/  # artefakty przebiegów
<cache>/project-studio/<project_id>/sources/<source_id>/  # workspace git/ZIP (odtwarzalny)
```
- Pool per projekt: cache otwartych pooli (LRU + zamykanie bezczynnych, checkpoint WAL przy zamknięciu). Wzorzec pojedynczego poolu: `ml_studio/db.rs` (OnceLock, WAL, wersjonowane MIGRATIONS). Referencje tożsamości (`owner_user_id`, `org_id`) jako TEXT, ZERO FK do `tentaflow.db`.
- Katalogi wg `tentaflow-core/src/paths.rs`: trwałe pod `data_dir()`, ciężkie/odtwarzalne (workspace, cache eksportów) pod `cache_dir()` (podąża za override na inny dysk).
- Eksport projektu = spakowanie katalogu `projects/<id>/` + wiersza rejestru (manifest.json + sha256, guardy zip-bomb) — wzorzec `ml_studio/project_archive.rs`. Workspace git NIE wchodzi (odtwarzalny). Import = rozpakowanie + wpis + re-map właściciela (`ml_studio_remote_import.rs`).

## 3. Warstwa wektorowa — izolacja per projekt (potwierdzone)

- Zvec jest path-based: `ZvecBackend::open_or_create(file_path, …)` (`services/vector/zvec_backend.rs:110`) i `Collection::create_or_open(path, …)` (`tentaflow-zvec/src/lib.rs:279`) otwierają kolekcję w DOWOLNYM katalogu. Ścieżka jest utrwalana per wiersz w `addon_vector_namespaces.file_path` i odczytywana przy otwarciu (nie przeliczana).
- **Kwoty** `MAX_NAMESPACES_PER_ADDON=10`, `MAX_VECTORS_PER_ADDON=1_000_000` (`services/vector/namespace.rs:99,105`) liczone per `(org_id, addon_id)`. Rozwiązanie: użyj **scope per projekt** `addon_id = "ps-<project_id>"` (walidacja charsetu przechodzi) → limity obowiązują PER PROJEKT.
- Do zapisu w katalogu projektu dodaj wariant `NamespaceManager::get_or_create_at(custom_dir)` (rozmiar S — ścieżka i tak jest utrwalana per wiersz; trzeba tylko podstawić katalog przy tworzeniu, `namespace.rs:702`). Retrieval przez ten sam scope.

## 4. Ingest wiedzy (reużycie istniejącego)

- Ekstrakcja: `services/document/extract.rs` (klasyfikacja po MIME+magic; PDF/Office/tekst), rasteryzacja skanów `services/document/rasterize.rs` (pdfium) + VLM parse, chunking (`node_adapters/chunk.rs`). Embeddingi przez alias/runtime.
- **Nie używaj węzła flow `store`** — twardo wymaga `ctx.addon_id` (`flow_engine/node_adapters/store.rs:52`). Ingest Projektów to natywny job core, który woła `NamespaceManager::upsert_batch_with_quota` wprost (semantyka cleanup-then-reingest powtórzona ze store.rs).
- **Kod źródłowy**: klasyfikacja jest MIME-only (kod jako `application/octet-stream` → dziś twardy błąd). Dodaj mapę rozszerzeń → typ tekstowy + prosty chunker liniowy z nagłówkiem ścieżki pliku (tree-sitter = później). Parser OpenAPI: dodaj `openapiv3` (dziś brak parsera — jest tylko generator `api/openai/openapi.rs`).
- **Git**: jedyny git w runtime to `deploy/python_venv.rs` (`git clone --depth 1` przez systemową binarkę). Powiel wzorzec dla źródła repo: clone do workspace projektu, „Odśwież" = fetch + fast-forward + re-ingest delty po sha plików. Token repo jako sekret (SettingsCipher). Ekrany: W01 (statusy/Odśwież/Anuluj), W02 (formularz git), W04 (drzewo+podgląd).

## 5. Blok flow + narzędzia agentów (UC2)

- Nowy węzeł core `project_knowledge` (kategoria „Projekty" w palecie Flow Buildera): operacje `search|list_artifacts|get_document|list_cases|run_summary|list_tasks|list_sources`. Rejestracja adaptera w rejestrze `flow_engine/dispatcher.rs`, seed szablonu bloku (node_type/category/params_schema) w `db/seed.rs`. dynamic_enum źródło `projects` wymaga gałęzi w `www/js/modules/flows-builder/config.js` (loadDynamicEnumOptions) + request listy projektów per sesja.
- Narzędzia agentów `core.project_*` (buildiny): rozszerz `agents/builtins.rs` (CoreToolName + spec + execute). UWAGA: `execute_core_tool` nie dostaje dziś tożsamości usera — rozszerz sygnaturę o principal/user_id (dane są w `tool_exec` przez `ctx.user_id`/`AgentPrincipal`) i dołóż uchwyt `project_studio::db::pool()`. `user_id=None` (klucz `general` przez /v1) → odmowa. Członkostwo w projekcie sprawdzane przy każdym wywołaniu; audyt przez AiGateway.

## 6. Serwis test-runner (wykonanie testów pisanych przez agentów)

- Rejestracja: `tentaflow-containers/tools/_services/test-runner.toml` (kategoria `tools` — enum kategorii jest zamknięty w `services/manifest/types.rs`; „testing" tylko jeśli rozszerzysz enum). Deploy: docker + native python-bundle (wzorzec serwisów treningowych `training/_services/ml-training.toml`).
- Obraz bazowy: Python + pytest + Playwright + Locust + httpx. Dodatkowe toolchainy (Node/.NET/JVM/Rust) jako warianty/warstwy — runner deklaruje w `/health`, jakie ma; core przy starcie przebiegu sprawdza `language` przypadku (brak toolchainu → item `skipped`, kod do eksportu).
- Kontrakt HTTP: `POST /runs` → job_id, `GET /runs/{id}/status` (inkrementalnie), `POST /runs/{id}/cancel`, `GET /health`. Wyniki znormalizowane: junit-xml (pytest/jest/xUnit/JUnit) + artefakty; Locust → statystyki (p50/p90/p99, RPS, error-rate). Unit-testy na kodzie: `build_profile` per źródło (base_image/install_cmd/test_cmd) — agent proponuje, człowiek zatwierdza.
- **Sandbox**: kontener bez dostępu do hosta, limity CPU/RAM/czasu, **sieć TYLKO do allowlisty hostów zatwierdzonego środowiska** (route-guard Playwright + wrapper socket/HTTP), sekrety środowiska tylko w pamięci runa. To odwraca problem SSRF: nie „publiczny internet" (jak browser-renderer), lecz „dokładnie ten zatwierdzony target".
- Discovery: `services::list_by_category(conn,"tools",Some("test-runner"))`; brak wyniku → sprawdź tabelę bez filtra statusu, żeby odróżnić „wdróż" od „uruchom zatrzymany" (ekrany T08/T12 to pokazują).

## 7. Długie zadania (przebiegi, generowania, ingest) — wzorzec potwierdzony

- Wzorzec spawn+poll (omija `DISPATCH_TIMEOUT=30s`, który dotyczy tylko `service_call::dispatch`, nie bezpośredniego HTTP): handler tworzy wiersz runu ze statusem `running`, `tokio::spawn`, natychmiast zwraca `run_id`; task w tle robi POST + poll GET co 3–5 s w `spawn_blocking`; metryki inkrementalnie do DB; panic-guard na JoinHandle → status `failed`. Wzorce: `ml_studio/train_llm.rs`, `benchmark/runner.rs`.
- **Live log/postęp**: współdzielona szyna `deploy::log_bus` (klucz = run_id) + osobny streaming handler w `dispatch/stream_handlers.rs` (wzorzec `BenchmarkRunStreamRequest`). Kanał twórz PRZED spawnem (race z natychmiastową subskrypcją). Front: `ApiBinary.subscribe(...)` + poll wyników co 4 s. NIE pisz logów do SQLite.
- **Watchdog** (ekran T10): N nieudanych polli z rzędu → status przebiegu `error` „utracono kontakt z test-runner", częściowe wyniki zachowane; akcja „Oznacz jako błędny".
- Wyniki persystowane inkrementalnie w JEDNEJ transakcji per poll (nie per item). Cancel zachowuje częściowe wyniki (wzorzec `benchmark/runner.rs:171`).
- **Generowania** (ekran T05): to runy agentów przez Agent Harness (`AgentRunManager`), postęp przez `ProgressBroker`/`AgentRunEvent` + gotowy widget `tf-agent-activity`. Ścieżka **blocking** dla wywołań LLM z narzędziami (bo streaming+tools na modelach embedded nie działa — `services/runtime/executor.rs` `dispatch_chat_stream`). Provenance → `provenance_json` + zasil `compliance_ai_sources` (funkcja `add_ai_source` istnieje, dziś nikt nie woła).

## 8. Chat projektu — ważna pułapka (ekran C01)

- Flow `ps-chat`: `trigger → project_knowledge (deterministyczny RAG, NIE tool-call) → conversation_history → llm STREAMING BEZ narzędzi`. Cytowania budowane z metadanych węzła RAG, nie z tool-calli. Dzięki temu pełny token-streaming działa też na modelach embedded. Wariant z narzędziami w chacie (faza blocking + osobny finalny streamingowy call) = v2.
- Flow systemowe `ps-*` wymagają migracji kolumny `is_system` w tabeli `flows` (dziś brak — `FLOW_COLS` w `repository.rs:4034`) + filtr list + blokada edycji/usunięcia + idempotentny re-seed (wzorzec `is_system` z tabeli `prompts`).

## 9. Uprawnienia (ekrany X03, X04, T12, A0x)

- Org: `project_studio.read` (wszystkie role) + per-user grant „może tworzyć projekty" (tabela `project_creator_grants`, checkbox w ekranie Użytkownicy). Wzorzec par uprawnień: migracja `roles_add_permissions` (`db/migrations.rs`), egzekwowanie `ctx.org_context.has(PERM)` (wzorzec `dispatch/benchmark.rs:31`).
- Projekt: role `owner/manager/editor/tester/viewer` w `project_members` (wzorzec `ml_studio` `project_members` + guardy `require_project_{owner,editor,member}`). Macierz ról = spec §4.2.
- **ML Studio governance z projektu** (ekran X02): tworząc/łącząc projekt ML mapuj role przez `role_map`; **znieś wymóg power-user** dla członkostw z projektu (dziś `require_invitee_power_user` w `dispatch/ml_studio.rs:309` blokuje). Podsumowanie projektu ML czytaj wprost z `ml_studio.db`.
- Środowiska auto/perf z adresem prywatnym/LAN wymagają zatwierdzenia admina (`#[policy(Admin)]`, kolejka — ekran T12) — model zaufania jak `network_rule` addonów. Manualne przebiegi nie wymagają środowiska ani zatwierdzenia.

## 10. Komponenty UI do dobudowania (poza istniejącymi tf-*)

Istnieją już (potwierdzone w `www/js/components/`): tf-table, tf-window/tf-modal, tf-tabs, tf-line/bar/area/pie-chart, tf-sparkline, tf-heatmap, tf-gauge, tf-stat-card, tf-agent-activity, tf-file-input, tf-tree, tf-progress-bar, tf-chip/status-pill, tf-searchbox, tf-filter-chips, tf-detail-header, md-lite. Do dobudowania:
- **tf-code-editor** (ekran T03) — własny, bez zewnętrznych bibliotek: tokenizer (python/js/ts/json/yaml/md/gherkin), numeracja, zwijanie, szukaj/zamień, wirtualizacja linii, panel AI (generuj/popraw/diff, streaming). Rozmiar L — największy klocek frontowy.
- **tf-kanban** (ekran Z01) — kolumny statusów + drag&drop.
- **Paginacja w tf-table** (duże listy przypadków).
- **tf-chip-input** z autouzupełnianiem (tagi).
- Panel dzwonka/powiadomień (ekran G02) + tabela `notifications` + toasty przez `SystemEventBody`.
Uwaga: **brak tf-drawer** — panele boczne rób jako tf-window (mockupy tak robią).

## 11. Mapowanie ekran → realizacja (skrót)

| Ekran | Backend / tabele | Uwagi |
|---|---|---|
| P01 lista, P02 kreator | projects.db: projects, project_members, creator_grants | grant tworzenia gatuje „Nowy projekt" |
| P03 przegląd | agregaty z project.db + activity_log | KPI liczone zapytaniami |
| W01–W04 wiedza | sources, source_files + vectors/ (scope ps-<id>) | git connector + ingest job + parser OpenAPI |
| T01–T03 przypadki | test_cases, test_case_versions | edytor kodu = tf-code-editor + AI (blocking) |
| T04–T05 generowanie | generation_runs + Agent Harness | tf-agent-activity, provenance→compliance_ai_sources |
| T06 zestawy | test_suites, suite_cases | blokada usunięcia gdy w harmonogramie |
| T07–T11 przebiegi | test_runs, test_run_items, test_run_steps + runs/ | spawn+poll+log_bus+watchdog; runner |
| T12 środowiska | environments | zatwierdzanie admina (auto/perf) |
| T13 harmonogramy | schedules | pętla poll modułu (wzorzec Admin Scheduler) |
| T14 raporty | zapytania agregujące + chart-svg | pokrycie z linked_sources_json |
| D01–D02 dokumenty | documents, document_versions | „Popraw z agentem" = blocking |
| C01 chat | project_chats + conversation_messages | ps-chat: RAG-węzeł + streaming bez tooli |
| Z01–Z02 zadania | tasks, task_comments | usterki = typ zadania; prefill z pulpitu testera |
| (brak zakładki Artefakty) | pochodzenie jako atrybut obiektu | wygenerowane pliki żyją przy obiektach: raport przy przebiegu, kod przy przypadku/generowaniu, eksport przy przebiegu/dokumencie; pobrania signed URL wzorcem ML Studio |
| X02 ML | ml_links + odczyt ml_studio.db | role_map + zniesienie power-user |
| X03 członkowie | project_members | transfer własności |
| X04 ustawienia | projects.settings_json + tagi | agenci per funkcja (aliasy), retencja |
| A01–A05 agenci (UNIWERSALNI) | tabela `agents` (istnieje) | przebudowa UI; model = combobox z pełnej listy modeli/aliasów (nie 3 kafelki); wybór narzędzi ORAZ źródeł wiedzy/RAG; wbudowany asystent budowy (własny system prompt); playground = blocking harness; brak draft/publish (enabled/disabled) |

## 12. Kolejność realizacji (etapy — spec §14)
E1 fundament (składowanie per projekt, wiedza dokumenty+URL, chat, project_knowledge + core.project_*, przegląd, audyt). E2 testy manualne + generowanie (pełne demo UC1: T01-T02, T04-T09, Z01-Z02, powiadomienia, raporty 1-3+5+6). E3 automatyzacja (test-runner, T03 edytor+AI, T10-T12, spec API+git, perf minimalny). E4 rozszerzenia (harmonogramy, kanban, ML integracja, dodatkowe toolchainy, eksport/share).

## 13. Pułapki z kodu (nie powtarzać błędów)
1. `MessageBody` na granicy 256 wariantów → jeden wariant z pod-enumem, dodawaj na końcu.
2. Węzeł flow `store` wymaga addon_id → ingest pisze przez NamespaceManager wprost.
3. Kwoty wektorów per (org, addon_id) → scope per projekt `ps-<id>`, inaczej ~10 projektów/org.
4. Streaming + tool-calling na modelach embedded NIE działa → chat bez tooli (RAG-węzeł), generowania na ścieżce blocking.
5. `flows` nie ma `is_system` → dodać migracją, inaczej flow systemowe będą edytowalne/usuwalne przez usera.
6. `execute_core_tool` bez tożsamości usera → rozszerzyć sygnaturę o principal.
7. Kategoria serwisów to zamknięty enum → test-runner w `tools`.
8. `compliance_ai_sources` istnieje ale nieużywane → zasilić dla provenance.
9. Klasyfikacja dokumentów MIME-only → kod potrzebuje mapy rozszerzeń + chunkera.
10. Brak parsera OpenAPI → dodać `openapiv3`.
