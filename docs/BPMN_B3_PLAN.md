# Plan techniczny BPMN, faza B3 (edytor, implementacje zadania usługi, weryfikacja, import Camundy, symulacja)

Data analizy: 2026-10-12. Analiza jest tylko do odczytu, żaden kod nie był zmieniany. Odnośniki `plik:linia` pochodzą z odczytu repozytorium w tej sesji. Gdzie czegoś nie sprawdziłem, piszę to wprost jako „do zweryfikowania”.

## 0. Najważniejsze ustalenia z kodu (wpływ na projekt)

1. **Zadanie usługi zna dziś tylko przepływ.** `ProcessNodeKind::ServiceTask` ma pola `flow_id`, `input_mapping`, `output_mapping`, `verification`, `timeout_seconds`, `result_expression` (`tentaflow-protocol/src/processes.rs:630-638`). `validate_model` wymaga niepustego `flow_id` i `timeout 1..=600` (`processes/model.rs:1350-1376`).
2. **Cały mechanizm wykonania jest zbudowany wokół przypiętego grafu przepływu.** Elementy, na których to się opiera:
   - Publikacja przypina graf przepływu per węzeł (`dispatch/processes.rs:285-307`). Wynik trafia do `bpmn_versions.service_snapshots_json` (`db/migrations.rs:1456-1465`).
   - `execute_claimed` szuka migawki po `node_id` i `flow_id` (`processes/jobs.rs:241-246`).
   - Następnie woła `authorize_process_flow` (ACL przepływu + kontekst organizacji, `flow_engine/dispatcher.rs:1054-1087`) i `dispatch_pinned_flow` (`jobs.rs:350`, `dispatcher.rs:1155-1193`).
   - Tabela `bpmn_service_invocations` ma `flow_id TEXT NOT NULL` i `pinned_flow_digest` o długości 64. Wiersz jest niezmienny po zapisie (trigger `NEW.flow_id IS NOT OLD.flow_id`, `migrations.rs:19983-20097`).
3. **Przypięty przepływ nie może zawierać agenta.** `validate_process_snapshot` odrzuca `subflow`, `map`, `agent`, `agent_router`, `spawn`, `await_subagents` (`dispatcher.rs:1135-1146`). Agenta w procesie nie da się więc zrobić „przez przepływ”. Potrzebna jest osobna ścieżka przez `AgentRunManager`.
4. **Kontrakt wyniku istnieje i działa.**
   - `ActivityResult {outcome, code, summary, outputs, evidence}` (`processes.rs:1633-1647`).
   - Pochodzenie wyniku: `Envelope` / `Contract` / `Platform` (`jobs.rs:75-121`, `parse_contract_result` w `jobs.rs:122`).
   - Zdarzenia brzegowe błędu i eskalacji reagują tylko na wynik o pochodzeniu `Contract` (`runtime.rs:7221-7244`).
   - Dlatego dziś `BoundaryEscalation` wymaga na zadaniu `result_expression` (`config.js:608`, `model.rs:1731`).
   - Brak wyniku `NeedsHuman` otwiera zadanie weryfikacji człowieka (`runtime.rs:7639`). To już realizuje zasadę „brak jawnego wyniku nigdy nie jest sukcesem”.
5. **Weryfikacja ma dziś 2 tryby.** `ActivityVerification { Condition{expression}, Human }` (`processes.rs:819-826`), obsługiwane w `runtime.rs:7272, 7290, 7638-7736`. Fałszywy warunek daje incydent `VERIFICATION_FAILED`, nie błąd do obsłużenia na brzegu.
6. **Agenci mają gotowy wzorzec trwałego ujścia.**
   - `core.project_case_save` z wiązaniem `envelope.meta["ps_generation"]`, wstawianym przy `AgentRunManager::spawn(..., extra_meta, ...)` (`agents/run_manager.rs:412-435`, `project_studio/generation.rs:19`, `tool_exec.rs:311-373`).
   - Wzorzec spawnu: `dispatch/project_studio.rs:8631-8648`.
   - Obserwator: `await_run` (`run_manager.rs:907`) + `spawn_watcher` (`generation.rs:1143`).
   - **Przebiegi agentów nie przeżywają restartu.** `reap_interrupted_on_startup` zamyka je jako `interrupted` (`run_manager.rs:329`). Przy `await_run` rząd bez żywego zadania daje błąd (`run_manager.rs:917-932`).
7. **Narzędzia addonów:**
   - `AddonManager::call_tool(addon, tool, params, user)` jest **synchroniczne** (blokujące WASM). Sprawdza uprawnienie „llm” użytkownika (`addon/mod.rs:3133, 3258-3270`).
   - Istnieje precedens znakowania pochodzenia: `call_tool_scheduled` (`addon/mod.rs:3206`).
   - Wywołania WASM nie da się przerwać z zewnątrz. Można je tylko porzucić po stronie `tokio`.
8. **Praca jobów w runtime:** pętla bierze maks. 4 joby naraz (`runtime.rs:8301`). Job agenta trwający kilkanaście minut zajmuje slot. Trzeba to rozdzielić klasami.
9. **Import XML jest ścisły i kończy się na pierwszym błędzie.**
   - Parser rozumie tylko przestrzenie `bpmn:` i `tentaflow:`.
   - `children_only` / `attrs_only` odrzucają wszystko inne (`bpmn.rs:151-162, 2484-2558`).
   - `import_xml` zwraca `None` + **jedną** fatalną diagnostykę (`bpmn.rs:3820-3847`).
   - Rozszerzenia `camunda:` występują w kodzie tylko w jednym teście (`bpmn.rs:6894`).
   - Import Camundy wymaga więc **osobnego, tolerancyjnego trybu**, a nie dopisania wyjątków do ścisłego parsera.
10. **Symulacja jest wąskim profilem z wyraźną listą dozwolonych planów.**
    - `PROFILE_NAME = "script_user_manual"` (`simulation.rs:36`, CHECK w `simulation_schema.rs:40`).
    - Lista dozwolonych typów węzłów: `simulation.rs:2250-2316`.
    - `validate_supported_plan` (`simulation.rs:2391-2465`) odrzuca joby, wiadomości, sygnały, subskrypcje, scope'y, wywołania, powtórzenia, błędy biznesowe i zadania weryfikacji.
    - Baza symulacji jest prywatna i budowana z kodu (`SimulationDatabase`, szablon w `OnceLock`). **Zmiana profilu nie wymaga migracji produkcyjnej.**
11. **UI:**
    - Paleta trybu `bpmn` używa wyłącznie `processTemplates()` z `bpmn.js:5-45`, a kategorie są zakodowane na sztywno `['events','tasks','gateways']` (`palette.js:101, 178`).
    - Inspektor zadania usługi to płaska lista pól w `config.js:630-643`. Nie ma zakładek.
    - Ręczny kodek `www/js/protocol/codec.js:667-677` (+ `process-wire.test.js`) i klucze `bpmn.*` w 5 językach (`www/i18n/pl.json`, `en.json` od linii 15513, `de/es/fr` od 14278).
    - Brak specyfikacji e2e dla BPMN (`tests/e2e/*bpmn*` nie istnieje). Testy to `bpmn.test.js` (węzłowe) i testy Rust.
12. **Ograniczenia kompatybilności, które determinują kształt zmian:**
    - Opublikowane modele są przypięte sumą SHA-256 i migracja 198 odmawia przy zmienionym modelu (patrz §10.2a planu). **Każde nowe pole modelu musi mieć `#[serde(default, skip_serializing_if = …)]`, tak by stare modele serializowały się bajt w bajt.**
    - `ProcessNodeKind` ma `deny_unknown_fields`. Stary binarek odrzuci nowe pola, co jest akceptowalne (reguła z CLAUDE.md: węzły mesh przebudować razem, `SCHEMA_VERSION` 32 nie podnosimy).
    - Migracje: następna wolna to **201** (B3.1); rezerwacja TentaBus B7 przesunęła się na kolejny wolny numer. Drabina musi być ciągła (test `migration_ladder_is_contiguous_and_applies_each_version_once`), więc luki nie wolno zostawić. Numery 201+ przydzielać w momencie implementacji i sprawdzić kolizje z `origin/main`.
    - `ServiceTask { .. }` jest dopasowywane wyczerpująco w wielu miejscach. Liczba trafień w `processes/*` i dispatch to około 214, głównie testy. Kompilator wskaże wszystkie.

## 1. Projekt wspólny (fundament dla pozycji 1-4)

### 1.1 Model: implementacja zadania usługi

W `tentaflow-protocol/src/processes.rs` dopisać (dopisywanie wariantów jest bezpieczne, tagi po NAZWIE):

```text
ServiceTask { ...dotychczasowe pola...,
  #[serde(default, skip_serializing_if = "ServiceImplementation::is_flow")]
  implementation: ServiceImplementation }

enum ServiceImplementation {
  Flow,                                   // dotychczasowe zachowanie, flow_id niepuste
  AddonTool { addon_id, tool_name },      // argumenty = input_mapping (CEL)
  Block { node_type, config: Value },     // jeden blok Flow Buildera
  Agent { agent_id, goal_expression, allowed_tools: Vec<String>,
          max_iterations: u32, output_schema: Option<Value> },
  Unbound { origin: String },             // tylko szkic z importu Camundy, bez publikacji
}
```

- Reguła walidacji w `model.rs`: dla `Flow` wymagane niepuste `flow_id`. Dla pozostałych `flow_id` musi być puste. Dla `Agent` limit czasu to `1..=3600`, dla reszty zostaje `1..=600`.
- `Unbound` przechodzi `validate_model` (zapis szkicu), ale publikacja go odrzuca. Dzięki temu import Camundy może zapisać szkic.
- Nowe warianty `ActivityVerification`: `Test { flow_id, on_failure }`, `Critic { agent_id, criteria, on_failure }`, gdzie `on_failure ∈ {Error, NeedsHuman}`. Wariant `Human` zostaje domyślny.
- Odrzucone alternatywy:
  - `ServiceTaskV2` łamie regułę o wersjonowanych nazwach.
  - `flow_id` jako `Option` zmienia bajty starych modeli.
  - Pakowanie bloku/agenta do przepływu systemowego wymagałoby ukrytych wierszy `flows`, ACL i synchronizacji flowów po mesh.

### 1.2 Przypięcie przy publikacji

- `PinnedServiceSnapshot` (zapisywany w `service_snapshots_json`) dostaje dwa pola z `#[serde(default)]`:
  - `role` (`implementation` | `verification`). Jest potrzebne, bo `jobs.rs:243` szuka po `node_id` + `flow_id`, a węzeł może mieć 2 migawki.
  - `implementation_kind`.
- Co jest przypięte w `graph_json` / `graph_sha256`, dla każdej implementacji:
  - **Blok:** zsyntetyzowany w pamięci, deterministyczny graf `trigger → blok → output`. Jego JSON i SHA-256 są przypięte tak jak graf przepływu.
  - **Narzędzie:** kanoniczny JSON `{addon_id, tool_name, bundle_hash, input_schema_digest}`.
  - **Agent:** kanoniczny JSON `{agent_id, digest(system_prompt, model, tools_json, params_json, runtime_json), allowed_tools}`. Służy do zapisu w historii, nie do blokowania (decyzja 2 w §8).
- Migracja **201**:
  - `ALTER TABLE bpmn_service_invocations ADD COLUMN implementation_kind TEXT NOT NULL DEFAULT 'flow'`.
  - Kolumna `flow_id` zostaje i dla nie-flow niesie stabilną referencję implementacji (`tool:<addon>/<tool>`, `block:<type>`, `agent:<id>`). Ograniczenie długości digestu 64 jest spełnione.
- Do sprawdzenia przed implementacją: pozostałe triggery i CHECK-i w migracji 198 (`migrations.rs:19983-20100`) pod kątem założeń o `flow_id`.
- Publikacja (`dispatch/processes.rs:285`) rozgałęzia się po `implementation` i zbiera także migawki weryfikacji.

### 1.3 Wykonawca

`jobs.rs::execute_claimed` rozdzielić na `execute_flow` (obecny kod) oraz `execute_tool` / `execute_block` / `execute_agent`. Wspólne zostają: claim, odnawianie dzierżawy (`LEASE_RENEWAL`), ogrodzenie próby, `commit_job_dispatch_boundary`, `record_job_observation`, tabela faz wywołania. Dzięki temu semantyka „co najwyżej raz” i `uncertain` po restarcie jest dziedziczona bez zmian w plannerze.

Każdy wykonawca przed i w trakcie wywołania ponownie sprawdza autoryzację, jak `authorize_process_flow` w `jobs.rs:258, 361`. Błąd autoryzacji daje `SOURCE_ACCESS_REVOKED`.

### 1.4 Autoryzacja, organizacja, budżety, audyt (przekrojowo)

| Obszar | Reguła |
|---|---|
| Tożsamość | Zadanie działa jako inicjator instancji (`claimed.actor`, jak dla przepływów). Uprawnienia agenta/narzędzia nie są większe niż inicjatora. |
| Organizacja | `resolve_org_context(db, user, Some(org))` przy publikacji, claimie i odnowieniu dzierżawy (jak `dispatcher.rs:1067`). |
| Blok | Nazwa bloku na serwerowej liście dozwolonych (stała w Rust, `processes/blocks.rs`) i ścisły zestaw ustawień każdego bloku. Wdrożone: `llm`, `embeddings`, `pii_filter`. **Poza listą:** `agent*`, `spawn`, `await_subagents`, `subflow`, `map`, `loop`, `exec_command`, `delegate_cli`, `workspace_context`, `bus_*`, `ask_user`, a także `stt`, `tts` i `reranker` (ich porty niosą dźwięk albo JSON, którego proces nie ma jak podać ani odczytać; `template` i `transform` w Flow Builderze nie istnieją). |
| Narzędzie | Narzędzie istnieje, addon włączony, a inicjator ma uprawnienie „llm” addonu (`permission_checker.check(addon, user, "llm")`). Sprawdzane przy publikacji (publikujący), przy claimie i w odnowieniu (inicjator). Odmowa daje `NeedsHuman("TOOL_PERMISSION_DENIED")` z kartą zgody (§4.4 planu), a nie ciche przejście. |
| Agent | Agent włączony, runtime `llm` (agenci `cli` w B3 odrzucani przy publikacji). Dowolny agent może być krokiem procesu (decyzja 1 właściciela); obowiązują zwykłe reguły dostępu agenta i inicjatora. Wybór narzędzi: `agent.tools_json ∩ node.allowed_tools`, plus zawsze ujście `core.task_complete`. |
| Budżety | `timeout_seconds` per węzeł (agent do 3600 s), `max_iterations` węzła nie większe niż agenta. Bez dodatkowych limitów widocznych dla użytkownika (decyzja 4 właściciela): brak limitu kosztu, brak limitów jednoczesności systemowych i na organizację. Wewnętrzna klasa szeregowania w pętli `runtime.rs:8301` zostaje, by długie joby agentów nie zajęły wszystkich 4 slotów. |
| Idempotencja | Klucz `instancja·aktywność·próba` = istniejące `stable_request_id`. Agent: identyfikator przebiegu wyprowadzony deterministycznie z tego klucza (wymaga publicznego wariantu `spawn_with_run_id` w `run_manager.rs:443`, dziś prywatny). Narzędzie: dodatkowe pole `idempotency_key` w `request_json` (`addon/mod.rs:3288`). To jest addytywne, a narzędzia z efektem ubocznym mają go używać. Platforma nigdy nie ponawia po granicy wysyłki. |
| Ponowienie po restarcie | Bez zmian względem blokowego przepływu: job `running` po restarcie → `INTERRUPTED` → `uncertain` + incydent `EXTERNAL_OUTCOME_UNCERTAIN`. Ulepszenie dla agenta w B3.3: przy odzyskiwaniu adoptujemy zapisany wiersz ujścia, jeśli istnieje (§3.2). |
| Audyt | Zdarzenia `service_claimed` / `service_result` dostają `implementation_kind`, `agent_run_id`, skrót agenta/narzędzia, model. Powiązanie z audytem zgodności przez istniejący `agent_run`. Logi na tych ścieżkach niosą tylko identyfikatory i długości, nigdy treści (reguła CLAUDE.md dla danych osobowych). |
| Pochodzenie | Nowa wartość `FlowOrigin::Process` (obok `Scheduler`, `Project`). Do sprawdzenia konsumenci `as_str/parse/all_flow_origins` w `dispatcher.rs` (5 trafień dla `Scheduler`) i kolumny `origin` w analityce. Poza `bpmn_*` nie znalazłem CHECK-ów na `origin`. |

## 2. Pozycja 1: kategoria „Bloki TentaFlow” i upuszczanie jako zadanie usługi

### Stan
Paleta BPMN nie ładuje szablonów bloków (`palette.js:101`). `ELEMENTS` w `bpmn.js:5-35` jest statyczne. `OptionsResponse` niesie tylko `assignees` i `service_flows` (`processes.rs:1774-1777`, `dispatch/processes.rs:203`, limit 1000 pozycji).

### Projekt
- **Protokół (addytywnie, `#[serde(default)]`):** rozszerzyć `OptionsResponse` o `service_blocks`, `service_agents`, `service_tools`.
  - Typy: `ProcessOptionBlock{node_type,label,description,category}`, `ProcessOptionAgent{agent_id,name,description}`, `ProcessOptionTool{addon_id,tool_name,description,addon_name}` (`addon_name` = nazwa zainstalowanej instancji, przy braku nazwy id instancji) oraz `description` w `ProcessOptionFlow`.
  - Serwer filtruje te listy tymi samymi regułami autoryzacji co §1.4, więc klient nie oferuje niczego, czego i tak nie da się opublikować.
- **UI:**
  - `processTemplates()` (+ `palette.js`) dostaje szablony dynamiczne: `node_type: 'bpmn_service_task'`, kategoria `tentaflow`, `preset: { implementation: … }`. Mechanizm presetów (`presetTypes`, `palette.js:52`) już obsługuje wiele wpisów o tym samym `node_type`.
  - Do listy kategorii dopisać `'tentaflow'` (`palette.js:178`), plus klucz `bpmn.category_tentaflow` = „Bloki TentaFlow” w 5 językach.
  - Upuszczenie tworzy węzeł przez `processNodeConfig('ServiceTask')` połączony z presetem, z nazwą bloku/agenta/narzędzia jako etykietą i właściwą ikoną. Bez pytania o typ.
  - Podgrupy: Przepływy, Bloki, Agenci, Narzędzia addonów.
  - Pole „Otwórz przepływ” w zakładce Implementacja dla `Flow` (tani skrót). Pełna nawigacja „PROCES › PRZEPŁYW” z §1 planu jest poza B3.
- **Kodek:** gałąź `ServiceTask` w `www/js/protocol/codec.js:667-677` i `process-wire.test.js` (kodowanie `implementation`, wariantów weryfikacji, nowych pól `Options`).
- **Eksport/import XML** (`bpmn.rs:2484-2558`, `4620`): `<tentaflow:service implementation="tool|block|agent" …>` z elementami potomnymi. `flowId` obowiązkowe tylko dla `Flow`. Bajty XML dla `Flow` bez zmian, test roundtripu.

### Testy
- Rust: serializacja starych modeli bajt w bajt (złote dane), walidacja każdej implementacji, roundtrip XML.
- Węzłowe: paleta w trybie bpmn, drop z presetem, kodek.
- e2e `tests/e2e/process-b3-palette.spec.js` (nowy): upuszczenie bloku, agenta i narzędzia, zapis, ponowne otwarcie. **Dopisać do `playwright` projekt analogicznie do pozostałych specyfikacji; sprawdzić konfigurację przed pisaniem.**

### Ryzyka
- Lista 1000 pozycji i filtrowanie uprawnień per użytkownik przy każdym otwarciu palety (koszt `permission_checker` × narzędzia).
- Agenci i bloki mogą istnieć tylko na części węzłów mesh (instancją zarządza jeden węzeł). Publikacja na węźle A z agentem z B daje błąd przy publikacji (agent musi istnieć lokalnie).

## 3. Pozycja 2: implementacje inne niż przepływ

### 3.1 Narzędzie addonu (najprostsze, pierwsze)
- Wykonawca `execute_tool`:
  - Argumenty to wynik `input_mapping` (CEL) jako obiekt JSON, walidowany względem schematu narzędzia z manifestu, jeśli istnieje.
  - Wywołanie przez nowe `AddonManager::call_tool_process(addon, tool, params, user, provenance)` w stylu `call_tool_scheduled` (`addon/mod.rs:3206`) z `AddonCallProvenance::process(stable_request_id)`.
  - Uruchamiane w `spawn_blocking` i wyścigowane z `cancel`, timeoutem i odnawianiem dzierżawy.
- Wynik narzędzia → `ActivityResult{Completed, outputs: {"result": <json>}}`, pochodzenie `Envelope`. Błąd narzędzia → `Error("TOOL_ERROR")`, pochodzenie `Platform`: jak każdy błąd platformy omija `result_expression` i **nie jest przechwytywany przez zdarzenia brzegowe błędu ani eskalacji** (kończy się incydentem). Jawny wynik kontraktowy powstaje tylko z udanego wyniku przez `result_expression` (jak dla przepływu).
- **Zmiana narzędzia po publikacji (B3.1):** używane jest bieżące narzędzie (spójnie z decyzją 2 dla agentów), bez odmowy. Gdy skrót pakietu lub schematu wejścia różni się od przypiętego, zadanie zapisuje w historii zdarzenie `service_tool_drift` (przypięte i bieżące wartości) przed wywołaniem. Argumenty są sprawdzane względem bieżącego schematu. Osobny rodzaj zdarzenia, a nie dopisek do `service_claimed`, bo wyszukiwanie wywołania bierze najnowszy wiersz `service_claimed` danego ogrodzenia.
- **Odzyskiwanie po restarcie (B3.1):** zaobserwowany wynik narzędzia jest blokowany tylko przy jednoznacznej odmowie (utrata uprawnienia „llm”, utrata inicjatora). Gdy nie da się jeszcze rozstrzygnąć (addon jeszcze nie działa po restarcie, instancja chwilowo w innej organizacji, brak hosta), wynik zostaje w stanie `observed`, a pętla robocza co 5 s (strona po 64 wiersze, najdawniej sprawdzane pierwsze) ponawia sprawdzenie i go stosuje albo blokuje. Pierwsze nierozstrzygnięcie zapisuje jednorazowe zdarzenie `service_result_deferred`. Incydent `TOOL_RESULT_UNDECIDED` powstaje, gdy narzędzie przestało istnieć (od razu) albo po `UNDECIDED_DEADLINE_MS` (1 godzina); wynik pozostaje zapisany i zostaje zastosowany (incydent się zamyka), gdy odpowiedź wróci. Ta sama reguła obowiązuje żywego wykonawcę po obserwacji: tylko utrata uprawnienia „llm” lub inicjatora jest odwołaniem dostępu. Wiersz, którego odzyskanie się nie udaje, nie wstrzymuje innych ani startu węzła; po 5 nieudanych przebiegach z rzędu zgłasza incydent `RESULT_RECOVERY_FAILED`. Dziennik zawiera tylko identyfikator zadania i długość błędu. Incydent odzyskiwania zamyka się w tej samej transakcji, która stosuje (lub blokuje) wynik, więc nieudane zastosowanie zostawia go otwartego i nie tworzy kolejnych; zapis `incident_resolved` trafia do historii. Wyścig z równoległymi przejściami, który nie ustępuje, jest zwykłym oczekiwaniem (kolejny przebieg), a nie „addon niedostępny”. Wiersze, których nie ponawia przebieg okresowy (wynik przepływu, zadanie w toku), zgłaszają incydent już przy pierwszej nieudanej próbie przy starcie. Przebieg okresowy działa w `spawn_blocking`. Zadanie w kolejce, którego źródła nie da się odczytać, kończy się własnym incydentem `SERVICE_SOURCE_UNREADABLE` i nie blokuje kolejnych. Stan odłożenia zadania jest czyszczony przy przekroczeniu granicy wysyłki.
- **Addon musi działać (B3.1):** ścieżka procesu nigdy nie uruchamia addonu; zatrzymany, nieznany lub chwilowo w innej organizacji addon odkłada zadanie przed claimem (patrz „Limit porzuconych wywołań”: wycofanie i incydent po `DEFER_DEADLINE_MS`), a `SOURCE_ACCESS_REVOKED` oznacza wyłącznie utratę uprawnienia „llm” lub inicjatora. Organizacja instancji jest porównywana przy publikacji, przy claimie i tuż przed wywołaniem; błąd publikacji ma ten sam tekst dla addonu nieistniejącego, zatrzymanego i z innej organizacji.
- **Limit porzuconych wywołań (B3.1):** liczba wywołań WASM procesu w toku (także porzuconych po przekroczeniu czasu) jest ograniczona per addon (4). Pozwolenie żyje w wątku blokującym do prawdziwego powrotu gościa. Ponad limit (a także gdy addon nie działa) zadanie jest odkładane PRZED claimem: bez próby, ogrodzenia i zdarzenia `service_claimed`. Wycofanie to `min(1000 << n, 30000)` ms, gdzie `n` to liczba dotychczasowych odłożeń zadania (kolumny `bpmn_jobs.not_before_ms`, `defer_count`, `first_deferred_at_ms`, `last_defer_code`, migracja 202). Historia dostaje `service_deferred` tylko przy pierwszym odłożeniu i przy zmianie powodu. Po `DEFER_DEADLINE_MS` (10 minut) odkładanie kończy się widocznym incydentem (`TOOL_BUSY` / `TOOL_UNAVAILABLE`, zadanie bez skutku, ręczne ponowienie).
- **Anulowanie i timeout:** wywołanie WASM nie przerywa się. Po timeoutcie job kończy się `SERVICE_TIMEOUT`, a wątek blokujący dobiega końca w tle. Skutek jest już modelowany jako `uncertain`. Udokumentować.
- Odmowa uprawnień → `NeedsHuman`, patrz §1.4. Granica wysyłki i wynik odmowy zapisują się w jednej transakcji, więc awaria nie zostawia granicy dla wywołania, które się nie odbyło.
- Testy: narzędzie z fałszywego addonu (wzorzec testów `jobs.rs`), cofnięcie uprawnień w trakcie, restart między wysyłką a wynikiem, timeout.

### 3.2 Blok Flow Buildera
- Publikacja syntezuje graf `trigger → blok → output` (deterministyczny, kanoniczny JSON) i przypina go. Wykonanie przez nową funkcję dispatchera, np. `dispatch_pinned_synthetic`, **bez wiersza w tabeli `flows`**:
  - ACL przepływu jest zastąpione regułami z §1.4 (lista dozwolonych bloków + aktywny użytkownik + kontekst org).
  - Reszta ścieżki (`run_blocking`, `validate_process_snapshot`, kontrola SHA) bez zmian.
  - Ten sam `validate_process_result_binding`, jeśli ustawiono `result_expression`.
- Wejście: `payload`/`variables` z `input_mapping`. Wyjście: jak dla przepływu (`normalize_outcome`, `jobs.rs:42`).
- Testy: każdy blok z listy dozwolonych, odmowa bloku spoza listy, zmiana wersji kodu bloku nie zmienia przypiętego grafu, ograniczenie rozmiaru wyjścia (`validate_output`).

**Wdrożone w B3.2 (2026-10-13).** Odstępstwa od powyższego opisu są świadome:

- **Graf i pin.** `processes/blocks.rs` buduje `trigger (t1) → blok (b1) → output (o1)` z typu i ustawień bloku; JSON ma posortowane klucze, a jego SHA-256 jest pinem. Pin nie zawiera niczego z kodu bloku, więc zmiana kodu nie zmienia przypięcia (złote bajty i skróty dla każdego dozwolonego bloku). `PinnedServiceSnapshot.implementation_kind = block`, `flow_id = block:<typ>`. Publikacja (`repository::publish_definition`) wymaga, by pin był DOKŁADNIE grafem bloku z modelu, zbudowanym kodem z chwili publikacji. Przy uruchomieniu pin jest czytany PO STRUKTURZE (`parse_pinned_block`: trzy węzły trigger, blok, output i dwie zwykłe krawędzie, bez dodatkowych węzłów, warunków i konfiguracji triggera/outputu), sprawdzany jest SHA-256 zapisanych bajtów, a blok i ustawienia muszą być tymi z zadania w modelu. Nie porównujemy z tym, co zbudowałby dzisiejszy kod, żeby późniejsza zmiana `FlowNode` albo zawężenie listy nie unieważniły opublikowanych procesów. To, czy blok nadal wolno uruchomić, jest autoryzacją (blok zniknął z listy: `SOURCE_ACCESS_REVOKED`).
- **Brak migracji (dług przed B3.4).** CHECK kolumny `implementation_kind` z migracji 201 dopuszcza tylko `flow` i `addon_tool`, a przebudowa tabeli wywołań (wyzwalacze, klucze obce) nie jest tego warta. Wywołanie bloku zapisuje się więc jako `flow` z `flow_id = block:<typ>` (`ServiceImplementationKind::invocation_column`); kolumnę czyta tylko skan odzyskiwania wyników narzędzi. **Zapisane do zrobienia:** właściwy `implementation_kind = block` (i agent) wymaga przebudowy tego CHECK, czyli migracji 203 (kolejny wolny numer), przed B3.4; do tego czasu wiersz bloku jest rozpoznawany po prefiksie `block:`.
- **Lista i ustawienia.** Adaptery Flow Buildera nie mają `validate_config`, więc `validate_block` trzyma ścisłą listę dozwolonych kluczy z typami i zakresami (np. `llm`: `model` wymagany oraz `system_prompt`, `temperature`, `top_p`, kary, `max_tokens`, `reasoning_effort` jako `low|medium|high|xhigh|max`, `stop` do 4 napisów; bez `audio_output`, `model_meta_key`, `model_fallback`). `validate_block` działa przy publikacji (`validate_model` w `publish_definition` i w obsłudze publikacji) i przy budowie pinu (`block_pin`), NIE przy zapisie szkicu. Przy uruchomieniu ustawienia nie są walidowane ponownie (zawężenie schematu nie unieważnia opublikowanych pinów); jedyny wyjątek to blok z modelem, którego pin nie niesie niepustego napisu `model`: jest odrzucany jak zabroniony model, zamiast pominąć kontrolę ACL i skorzystać z modelu z envelope lub węzła. Krawędzie pinu muszą też mieć porty i `data_type` grafu kanonicznego. Model wymaga mapowania `payload` i pustego `flow_id`.
- **Wejście.** Bloki z listy biorą tekst, więc mapowanie `payload` musi dać napis. Inny typ kończy się `BLOCK_INPUT_INVALID` PRZED granicą wysyłki (nic nie uruchomiono, ponowienie bezpieczne).
- **Wykonanie.** `FlowDispatcher::dispatch_pinned_synthetic` kompiluje graf z pustym identyfikatorem, więc nie powstaje rekord `flow_executions` (FK do `flows`). Pochodzenie to `FlowOrigin::Process`. Autoryzacja (`authorize_process_block`, przy pinowaniu, przy claimie przed granicą i tuż przed uruchomieniem): blok na liście, aktywny inicjator, kontekst organizacji oraz dostęp inicjatora do modelu bloku (ten sam ACL `model` co w czacie i embeddingach; odmowa to `SOURCE_ACCESS_REVOKED`, a tekst odmowy nie zdradza istnienia modelu). Limit tokenów jest sprawdzany przed granicą i ma trzy odpowiedzi: wyczerpany to `TOKEN_QUOTA_EXCEEDED`, błąd infrastruktury samej kontroli odkłada zadanie (`BLOCK_UNAVAILABLE`), nigdy nie jest raportowany jako wyczerpany limit, a dla `FlowOrigin::Process` odmowa limitu jest też fatalna w `llm_impl` i w embeddingach (to samo dotyczy błędu startu zdarzenia audytu, ale tylko przy synchronicznym `dispatch_write`; przy asynchronicznym zapisie audytu błąd zapisu jest tylko logowany), które przechodzą przez bramę AI (audyt `compliance_ai_events` z organizacją procesu). Błąd odczytu konta albo limitu (`QuotaReadError`, typowany błąd `check_token_quota`, osobny od odmowy `RateLimitExceeded`) to `Unavailable`, nie odmowa; zwykła ścieżka czatu i przepływów zostaje fail-open, a dla `FlowOrigin::Process` brama w chwili wywołania (`with_strict_quota_reads`, LLM i embeddingi) też odmawia na nieczytelnym limicie: przed granicą zadanie jest odkładane (`BLOCK_UNAVAILABLE`, ta sama maszyneria co `TOOL_BUSY`), a po uzyskaniu wyniku sprawdzenie nie unieważnia go (publikację wyniku chroni sprawdzenie źródła w transakcji akceptacji, ale ono obejmuje listę dozwolonych i aktywnego aktora, NIE ACL modelu; pozostaje okno: dostęp do modelu odebrany po ostatnim sprawdzeniu przed uruchomieniem nie zatrzyma wyniku, który już istnieje). Jeśli limit stanie się nieczytelny albo wyczerpany między sprawdzeniem wstępnym a wywołaniem (już po granicy wysyłki), ścisła brama nadal odmawia przed wywołaniem backendu, ale zadanie kończy się jako `FLOW_ERROR`, a nie `BLOCK_UNAVAILABLE`/`TOKEN_QUOTA_EXCEEDED`. Przed uruchomieniem i po nim dispatcher odrzuca tylko odmowę jednoznaczną (inicjator utracony, blok zniknął z listy, model zabroniony); błąd odczytu nie zamienia przebiegu, który się nie zaczął, w niepewny skutek. Własna pętla ponowień bloku `llm` jest zawsze ustawiana na jedną próbę (klucz `max_request_attempts` nie jest oferowany): platforma nie powtarza żądania po granicy wysyłki.
- **Wynik.** Jak dla przepływu (`normalize_outcome`, `result_expression` daje wynik `Contract`). Błąd bloku po granicy wysyłki to `EXTERNAL_OUTCOME_UNCERTAIN`, nie wynik biznesowy.
- **Interfejs.** `synthetic.rs` w `flow_engine` nie jest modułem kompilowanym, więc graf powstaje w `blocks.rs`. Inspektor dostaje minimalną implementację „Blok Flow Buildera” (typ i ustawienia jako JSON); paleta i zakładki są w B3.3a/b. XML: `<tentaflow:service implementation="block" nodeType>` z `<tentaflow:blockConfig>` (JSON o posortowanych kluczach).
- **Kodek.** Klucz `config` jest nieprzezroczysty przy dekodowaniu (`process_json_to_js`), żeby `max_tokens` nie stało się `maxTokens`.

**Wdrożone w B3.3a (2026-10-15).** Odstępstwa od §2 są świadome:

- **Bez agentów.** `service_agents` i podgrupa „Agenci” nie powstały, bo wykonawca agenta jest w B3.4; paleta nie oferuje niczego, czego nie da się uruchomić. Dopisać w B3.4b (pole z `#[serde(default)]`, czwarta podgrupa).
- **Filtr narzędzi.** `FlowDispatcher::offered_process_tools` sprawdza inicjatora raz, a `ProcessToolHost::offered_tools` podejmuje decyzję addonu (działa, ta sama organizacja, uprawnienie „llm”, odczyt skrótu pakietu) raz na addon; ta sama funkcja `process_addon_access` obsługuje `authorize`, więc lista nie rozjedzie się z publikacją. Lista narzędzi jest ucięta po 1000 pozycjach w stałej kolejności (addon, narzędzie) bez błędu, w odróżnieniu od przepływów, które nadal odrzucają katalog większy niż 1000; obcięcie chowa skrót, nie możliwość, bo inspektor przyjmuje dowolny identyfikator narzędzia.
- **Bloki.** Etykiety i opisy serwer podaje po angielsku jako zapas; klient bierze tłumaczenia `bpmn.block_<typ>` i `bpmn.block_<typ>_hint`. Test pilnuje, by każdy blok z listy dozwolonych miał wpis w opcjach.
- **Ładowanie.** Opcje są pobierane razem z definicją przy otwarciu projektanta (jedno `Promise.all`, jak dotąd dla przepływów i osób), więc paleta dostaje je synchronicznie i nie ma osobnego asynchronicznego odświeżania; niepowodzenie nadal przerywa otwarcie. Gdy palety nie da się zbudować z opcji, grupa „Bloki TentaFlow” pokazuje komunikat, a pusta podgrupa komunikat „brak pozycji”.
- **Szablony.** Wpisy dynamiczne dzielą `node_type: bpmn_service_task` ze zwykłym elementem. Płótno zachowuje szablon zwykłego elementu (ikona BPMN, brak etykiety), a upuszczenie wpisu z `preset` scala preset z `processNodeConfig('ServiceTask')` i nadaje węzłowi nazwę pozycji.
- **Inspektor.** Lista typów bloków w inspektorze nadal jest stałą po stronie klienta (`PROCESS_BLOCK_TYPES`); przejście na `serviceBlocks` należy do B3.3b.
- **Odroczone z krytyki GUI B3.3a (G5).** Węzły zadania usługi z palety „Bloki TentaFlow” (przepływ, blok, narzędzie addonu) wyglądają na płótnie jak zwykłe zadanie usługi; odróżnienie rodzaju implementacji (ikona/plakietka na węźle) należy do B3.3b razem z inspektorem.
- **Odroczone z trzeciej rundy B3.3a (do B3.3b / B3.x).** (1) Projektant jest nieużywalny przy 1024x700: nagłówek zajmuje ok. 420 px. (2) Nowa aktywność wywołująca (Call activity) startuje z `calledVersion` 0, co blokuje zapis do czasu ręcznego wyboru wersji. (3) Instalacja drugiej instancji `notes` kończy się błędem „alias owned” i zostawia wyłączoną instancję. (4) Długa etykieta zadania zewnętrznego (external task) nie jest przycinana na węźle. (5) Etykiety portów węzła filtra PII nakładają się na siebie. (6) W angielskim UI zostały pojedyncze polskie napisy. (7) Odróżnienie rodzaju węzła na płótnie i ostrzeżenie o pustym modelu (jak wyżej, G5). (8) Zduplikowane nazwy przepływów nie są rozróżniane w palecie (brak sufiksu jak dla instancji narzędzi).
- **Odroczone z piątej rundy B3.3a (do B3.3b / B3.x).** (1) Indeks bazy pod stronicowanie przepływów (nazwa, id). (2) Unikanie adnotacji i pul przy szukaniu wolnego miejsca dla dodania z palety. (3) Limit nazwy instancji jest sprawdzany w bajtach (`handlers_addon_lifecycle.rs`), a komunikat mówi o znakach. (4) Płótno 144x115 px przy 1024x700.

**Wdrożone w B3.3b (2026-10-17).** Odstępstwa od §5 są świadome:

- **Zakładki.** Implementacja (wybór Flow/Narzędzie/Blok z parametrami), Wejścia i wyjścia (mapowania i dane wejścia/wyjścia aktywności), Wynik i błędy, Czas (tylko limit czasu; ponowień per aktywność nie ma w UI, są w B4) i Zaawansowane (powtarzanie i identyfikator BPMN). Wybór „Agent” nie powstał, bo wykonawca agenta jest w B3.4. Pole dokumentacji w Zaawansowanych nie powstało: model nie ma ogólnego pola dokumentacji dla zadania usługi (dokumentację niesie tylko zadanie ręczne), a jego dodanie zmieniłoby kontrakt modelu. Przełączanie krawędzi („Edytuj przepływ”, „Połącz z”) zostało pod zakładkami, bo dotyczy grafu, a nie zadania. Wybrana zakładka przetrwa ponowne pokazanie tego samego węzła.
- **Tryb weryfikacji.** Zakładka pokazuje dzisiejsze tryby (Człowiek, Warunek). Test i krytyk to B3.5.
- **Tabela „co się stanie” i silnik.** To czysta pochodna grafu po stronie klienta, bez reguł serwera, więc nie ma wspólnej tabeli Rust/JS. Reguły sprawdzono w silniku (`runtime.rs::plan_service_result`, `error_handler`; `jobs.rs::classify_result`, `fail_claim`; testy `real_contract_needs_human_selects_one_exact_or_catchall_escalation`, `timeout_abandons_the_wait_and_leaves_the_outcome_uncertain`):
  - Tylko wynik kontraktowy (niepuste `resultExpression`) może być Błędem, Wymaga człowieka albo Anulowanym. Awaria platformy (błąd przepływu, narzędzia lub wyrażenia) jest zawsze incydentem i żadne zdarzenie brzegowe jej nie łapie. Dlatego zdarzenie brzegowe błędu na zadaniu bez `resultExpression` nigdy się nie uruchomi (ostrzeżenie), a eskalacja bez niego jest odrzucana przy publikacji.
  - Błąd: zdarzenie z dokładnym kodem, potem zdarzenie bez kodu, potem (w podprocesie) zakres nadrzędny, inaczej incydent z kodem wyniku (`SERVICE_ERROR`, gdy go brak). Wymaga człowieka: eskalacja z dokładnym kodem, potem bez kodu, inaczej zadanie weryfikacji dla inicjatora. Anulowane: incydent. Warunek weryfikacji niespełniony: incydent `VERIFICATION_FAILED`.
  - **Odstępstwo od tabeli w §5:** przekroczenie czasu NIE trafia do timera brzegowego. Limit zadania (`timeoutSeconds`) po wysłaniu kończy się incydentem (powód `SERVICE_TIMEOUT`, skutek niepewny, bez ponowienia), a timer brzegowy chodzi na własnym zegarze od aktywacji. Tabela pokazuje je w osobnych wierszach, a ostrzeżenie pada, gdy timer nie jest krótszy niż limit zadania.
- **Przyciski obsługi.** „Dodaj obsługę błędu / eskalacji” dodają zdarzenie brzegowe (`FlowCanvas.addBoundaryNode`, jeden wpis cofania) z pierwszą wolną obsługą: najpierw bez kodu, potem kolejnym zadeklarowanym kodem; drugi uchwyt tego samego kodu odrzuca publikacja (`boundary_error_handlers`). Eskalacja jest wyłączona bez `resultExpression`. Nowe zdarzenie nie ma jeszcze przepływu wychodzącego (ostrzeżenie), a zaznaczenie zostaje na zadaniu.
- **Rodzaj węzła i model.** Plakietka (przepływ tylko z wybranym przepływem, blok, narzędzie addonu) i znacznik ostrzeżenia (blok z kluczem `model` bez niepustego napisu) są w `canvas.js`; funkcje `processServiceKind` i `processNodeWarnings` w `bpmn.js`.
- **Wersja wywoływanego procesu.** Draft przepuszcza wersję 0 przy pustej definicji, ale format przewodowy nie (`ProcessCallActivity` odrzuca `called_version` 0, a kodek JS wymaga `>= 1`), więc stanu „jeszcze nie wybrano” nie da się zapisać, a zmiana kontraktu jest poza zakresem. Domyślna wartość nie istnieje, więc nowa aktywność ma znacznik na węźle (`call_version`) i komunikat w inspektorze, który znika po wybraniu wersji. Dotyczy też zmiany trybu i definicji.
- **Nadal odroczone z B3.3a.** Użyteczność przy 1024x700 (1, 4), instalacja drugiej instancji `notes` (3), przycinanie etykiety zadania zewnętrznego (4), nakładanie etykiet portów filtra PII (5), polskie napisy w angielskim UI (6), sufiks nazw zduplikowanych przepływów (8), indeks stronicowania przepływów, unikanie adnotacji i pul, limit nazwy instancji w bajtach.

### 3.3 Agent z ujściem `task.complete`
Ujście proponuję nazwać **`core.task_complete`** (prefiks `core.` wymusza enum `CoreToolName`, `builtins.rs:24`, a istniejące `core.task_plan/update/list` należą do Code Studio, więc nazwa nie koliduje). Plan nazywa je `task.complete`, więc trzeba zmienić jego brzmienie przy aktualizacji dokumentu.

**Wiązanie serwera** (wzór `ps_generation`): `envelope.meta["process_activity"] = {invocation_id, instance_id, node_id, attempt, fence, role, allowed_tools, output_schema}`, przekazane przez `extra_meta` w `spawn` (atomowo, bez wyścigu z pierwszym wywołaniem). Model nie podaje żadnego z tych identyfikatorów jako argumentu.

**Ujście `core.task_complete`:**
- Argumenty: `outcome` (`completed|error|needs_human`), `code`, `summary`, `outputs`, `evidence[]`.
- Walidacja: limity jak w `parse_contract_result` (`jobs.rs:122-156`), `outputs` względem `output_schema` (podzbiór JSON Schema), kod `[A-Za-z0-9_.:-]{1,64}`.
- Sprawdzenie na żywo, że faza wywołania to `may_have_executed`, ogrodzenie się zgadza, instancja nie jest zamknięta, a identyfikator przebiegu jest tym utworzonym przez to wywołanie. Dzięki temu agent nie zakończy cudzego zadania ani zadania po anulowaniu.
- Zapis do nowej tabeli `bpmn_agent_completions` (migracja **203**): `invocation_id PRIMARY KEY`, `run_id`, `result_json`, `created_at_ms`. Pierwsze wywołanie wygrywa, drugie dostaje `[TOOL_ERROR] already completed`. Dzięki temu pętla agenta kończy się deterministycznie.
- Błąd walidacji jest `[TOOL_ERROR]` do naprawy przez model (jak `core.project_case_save`).

**Wykonawca `execute_agent`:**
- Tworzy `AgentPrincipal::new(Some(inicjator), Some(org), FlowOrigin::Process, FlowActor::user(inicjator))` z `correlation_id = instance_id`. Cel agenta to wynik CEL `goal_expression`. Dane z procesu są owijane jako ogrodzone DANE (ochrona przed wstrzyknięciem, jak `<<<PASSAGE>>>`).
- Czeka `await_run`, potem czyta wiersz ujścia → `ActivityResult`, **pochodzenie `Contract`**. Dzięki temu zdarzenia brzegowe błędu i eskalacji działają bez `result_expression`. Walidacje grafu w `config.js:608` i `model.rs:1731` trzeba rozszerzyć o „implementacja potrafi dać wynik kontraktowy” (`Agent` tak, `Flow/Block/AddonTool` tylko z `result_expression`).
- Przebieg zakończony bez ujścia, z budżetem wyczerpanym lub anulowany → `NeedsHuman("AGENT_NO_RESULT")` (zadanie weryfikacji człowieka, §4.2 planu).
- **Restart:** przebieg agenta jest `interrupted`. Przy odzyskiwaniu (`recover_jobs`, `repository.rs:26432`) najpierw szukamy wiersza w `bpmn_agent_completions`. Jeśli jest, przyjmujemy go jako zaobserwowany wynik bez ponownego wykonania. Jeśli nie, zostaje istniejąca ścieżka `uncertain` z incydentem. To jest jedyne miejsce, w którym zmieniamy semantykę odzyskiwania, i wymaga testu.
- **Zawężenie narzędzi na przebieg:** `tool_catalog_from_allowlist` dostaje `tools_json` przez zmienne (`agents/service.rs:168-201`). Punkt wpięcia to `agent_context.rs` (do zweryfikowania): przecięcie z `meta.process_activity.allowed_tools` oraz dołączenie `core.task_complete` tylko, gdy wiązanie istnieje. Bez tego nie da się zagwarantować niezależności krytyka (§4).
- Zasoby: slot klasy „agent” (§1.4). W trakcie czekania job odnawia dzierżawę (`LEASE_RENEWAL` 10 s).
- Agenci `cli` (`runtime_json`): odrzucane przy publikacji w B3 (ujście wymagałoby mostu MCP).

**Testy:** skryptowy `AgentRunner` (`AgentRunManager::new(db, runner, …)` przyjmuje abstrakcję) wywołujący ujście. Przypadki:
- sukces, `error`, `needs_human`
- brak ujścia
- podwójne ujście
- ujście po anulowaniu instancji
- obce `invocation_id` w próbie przekierowania
- restart z istniejącym i z brakującym wierszem ujścia
- limit czasu
- schemat wyniku naruszony → naprawa przez model
- wstrzyknięcie w danych procesu nie zmienia wiązania
- nieobsługiwany agent `cli` odrzucony

## 4. Pozycja 3: tryby weryfikacji „test” i „krytyk”

### Projekt (wariant A, rekomendowany)
Weryfikacja jest **drugim etapem tego samego joba**, wykonywanym po etapie pracy:
- **Test:** wykonuje przypięty przepływ weryfikujący (migawka z `role=verification`). Wejście: `payload` = wyjścia aktywności, zmienne = efektywne zmienne. Przepływ musi kończyć się blokiem „Wynik aktywności” (jest `validate_process_result_binding`). `Completed` = zatwierdzone.
- **Krytyk:** drugi przebieg agenta (`AgentRunManager`) z wiązaniem `role=verification`, własnym ujściem `core.task_complete` (`completed` = zatwierdzone, `error` = odrzucone z uzasadnieniem), `criteria` i wynik pracy w ogrodzonych DANYCH. Walidacja grafu: `critic_agent_id != agent_id`, krytyk dostaje wyłącznie narzędzia tylko do odczytu plus ujście (stąd zawężenie z §3.3).
- Werdykt zatwierdzający: `ActivityResult` zostaje `Completed`, a `evidence` dostaje `verified:test:<flow>` / `verified:critic:<agent>`. Odrzucający: wynik zamienia się w `Error("VERIFICATION_FAILED")` lub `NeedsHuman("VERIFICATION_FAILED")` według `on_failure`, pochodzenie `Contract`, oryginalne wyjścia zachowane w zdarzeniu historii. Dzięki temu odrzucenie można obsłużyć zdarzeniem brzegowym (§4.3 planu), a nie tylko incydentem.
- Planner (`runtime.rs:7272, 7290, 7638-7736`): nowe warianty traktują wynik `Completed` jako już zweryfikowany, a pozostałe wyniki idą istniejącymi ścieżkami. To nowe dopasowania, nie nowa maszyna stanów.

### Dlaczego wariant A
Planner i fazy `bpmn_service_invocations` zakładają jeden efekt zewnętrzny na job (`prepared → may_have_executed → observed → accepted`). Osobny job weryfikacji (wariant B) wymaga nowej fazy `awaiting_verification_job`, migracji i zmian w ~600 liniach plannera. Ryzyko jest tego samego rzędu co „Ponów dla zablokowanego mapowania”, którą odłożono do B4 (`BPMN_PROCESS_PLAN.md` §10.2a).

### Znane ograniczenie wariantu A
Awaria w trakcie etapu weryfikacji unieważnia wynik pracy (job → `uncertain`). Dla kosztownego agenta to realna strata, więc rozważyć wariant B jako B4 lub zapis pośredniej obserwacji etapu pracy przed weryfikacją. Dla etapu pracy typu agent można częściowo złagodzić przez adopcję wiersza ujścia (§3.3), a krytyka wtedy ponawiać bez ponawiania agenta pracy. **Do potwierdzenia na prototypie przed oddaniem B3.4.**

### Efekt uboczny „test”
Przepływ testowy musi być bez skutków ubocznych (nie da się tego wymusić). Walidator może odrzucać bloki spoza listy z §1.4, co daje częściowe zabezpieczenie.

### Inne
- Domyślny tryb (§9 pkt 2 planu): nowe zadanie agenta ma domyślnie „warunek”, jeśli istnieje `output_schema`, w przeciwnym razie „człowiek”. Pozostałe implementacje zachowują „człowiek”. UI wymusza wybór jawnie.
- Kombinacje trybów („warunek + człowiek” z §4.3) poza B3.

### Testy
Tryby × wyniki (zatwierdzenie, odrzucenie z `Error`/`NeedsHuman`, krytyk niezależny, test niedostępny), zdarzenia brzegowe łapią `VERIFICATION_FAILED`, ścieżka restartu. Złote bajty zdarzeń historii bez zmian dla trybów `Human`/`Condition`.

## 5. Pozycja 4: zakładka „Wynik i błędy”

### Stan
`config.js:630-643` renderuje wszystko płasko (flowId, mapowania, `resultExpression`, weryfikacja, timeout). Plan §1 przewiduje zakładki Implementacja · Wejścia i wyjścia · Wynik i błędy · Ponowienia i czas · Zaawansowane.

### Projekt
Zadanie usługi dostaje `tf-tabs`:
- **Implementacja:** wybór `Flow/Blok/Agent/Narzędzie` (z presetem z upuszczenia), parametry implementacji, timeout.
- **Wejścia i wyjścia:** `inputMapping` / `outputMapping` (istniejące `tf-keyvalue-editor`) i dotychczasowa sekcja IO aktywności.
- **Wynik i błędy** (nowa, wyłącznie z elementów istniejącego modelu):
  1. Źródło wyniku: `resultExpression` (przeniesione z płaskiej listy) lub ujście agenta.
  2. Tryb weryfikacji: Człowiek / Warunek / Test / Krytyk, z parametrami i `on_failure`.
  3. Tabela „co się stanie”, **wyliczana z grafu** (`processResultRouting(graph, nodeId)` w `bpmn.js`):
     - Ukończone → następny element.
     - Błąd(kod) → zdarzenie brzegowe błędu o tym kodzie, a jeśli brak, incydent.
     - Wymaga człowieka → zdarzenie brzegowe eskalacji, a jeśli brak, zadanie weryfikacji.
     - Anulowane.
     - Przekroczony czas → brzegowy timer, a jeśli brak, incydent `SERVICE_TIMEOUT`.
  4. Ostrzeżenia (np. kod błędu z `output_schema` bez obsługi, zdarzenie brzegowe na zadaniu bez źródła wyniku kontraktowego).
  5. Przyciski „Dodaj obsługę błędu / eskalacji” tworzące węzeł brzegowy.
- **Ponowienia i czas:** tylko pole czasu, bo ponowienia per aktywność są w B4 (nie obiecywać ich w UI).
- **Zaawansowane:** identyfikator BPMN, dokumentacja.
- Klucze `bpmn.*` w pięciu językach (test zgodności liczby kluczy); wyłącznie komponenty `tf-*`. Aktualizacja testu inspektora w `bpmn.test.js:1894` (selektory `data-process`).

### Ryzyka
Wymaga, by zakres „czy implementacja potrafi dać wynik kontraktowy” był wspólny dla Rust i JS. Trzymać jedną tabelę reguł w testach obu stron.

## 6. Pozycja 5: import rozszerzeń `camunda:` i tłumaczenie JUEL/FEEL

### Stan
Brak. Parser jest ścisły (§0.9). Referencyjny plik w repo: `docs/flows/project-studio-workflow-example.bpmn` (TentaFlow, bez Camundy). Pliki NextApp z planu (np. `Process_RackChange_ExternalApproval.bpmn`) nie są w repo, więc **potrzebujemy próbek od właściciela jako dane testowe** (patrz decyzja 5).

### Projekt
- Nowy tryb `import_camunda_xml(xml) -> (model, diagnostics, translations)`, uruchamiany po wykryciu przestrzeni `http://camunda.org/schema/1.0/bpmn`. Ścisły `import_xml` i jego zachowanie zostają nietknięte.
- Tryb tolerancyjny zbiera diagnostyki per element zamiast przerywać na pierwszej. `ProcessDiagnostic` ma już `fatal:bool` i pola kontekstu. Poziomy: `fatal` (element nie do zaimportowania), ostrzeżenie (utrata semantyki), informacja (zignorowano bez skutku). Kody z prefiksem `CAMUNDA_*`.
- `XmlImportResponse` dostaje `#[serde(default)] translations: Vec<ProcessImportTranslation{element_id, kind, original, translated}>`, żeby UI pokazało „przed → po” dla każdego wyrażenia.
- **Mapowanie konstrukcji:**

| Camunda | TentaFlow |
|---|---|
| `camunda:assignee` | `assignee_user_id`, o ile login rozwiązuje się do użytkownika; inaczej ostrzeżenie i inicjator |
| `candidateUsers` / `candidateGroups` / `formKey` / `formData` | brak odpowiednika w B3 → ostrzeżenie, pominięte |
| `camunda:inputOutput` (parametry tekst/wyrażenie) | `input_mapping` / `output_mapping`; listy, mapy i skrypty → ostrzeżenie |
| `serviceTask` z `delegateExpression` / `class` / `expression` / `type=external`+`topic` | `ServiceImplementation::Unbound{origin}`; publikacja zablokowana do ręcznego wskazania implementacji |
| `scriptTask` w JavaScript lub Groovy | fatal dla elementu (nie wykonujemy cudzych skryptów); tylko skrypty w wyrażeniu tłumaczalnym na CEL trafiają do `ScriptTask` |
| `multiInstanceLoopCharacteristics` z `camunda:collection` / `elementVariable` | `ProcessRepeatSpec::MultiInstance` |
| `executionListener` / `taskListener` | ostrzeżenie i pominięcie (mogą mieć skutki uboczne, więc lista widoczna) |
| `asyncBefore` / `asyncAfter` / `exclusive` / `jobPriority` / `historyTimeToLive` | informacja |
| `failedJobRetryTimeCycle` | ostrzeżenie (ponowienia per aktywność dopiero w B4) |
| `conditionExpression` z `language` ≠ JUEL/FEEL | fatal dla przepływu |

- **Tłumaczenie JUEL → CEL** w nowym `processes/expression_translate.rs`: czysty parser i generator, bez zależności zewnętrznych.
  - Zakres: `${…}` / `#{…}`, literały, operatory (`== != < > <= >= && || ! and or not eq ne lt gt le ge`, `+ - * / div % mod`), trójargumentowy `?:`, `empty x` → `size(x) == 0`, `a.b`, `a['b']`, identyfikator → `vars.<nazwa>`, `execution.getVariable('x')` / `variables.x` → `vars.x`.
  - Wszystko inne (wywołania metod beanów, `execution.setVariable`, funkcje `fn:`) trafia na listę do ręcznej poprawki.
  - Wynik przechodzi przez istniejące `expr::validate_syntax` (limity z `expr.rs`: 512 znaków, głębokość zagnieżdżenia).
  - **Różnica semantyczna:** JUEL koerce typy (np. tekst „7” do liczby w porównaniu), CEL jest ścisły. Każde porównanie, w którym typu nie znamy, dostaje ostrzeżenie „koercja typów”. Nie generujemy cichych rzutowań.
- **FEEL:** w Camunda 7 FEEL występuje głównie w DMN, a w BPMN rzadko. Zakres B3: mały podzbiór (`=`, porównania, `and/or/not`, literały, ścieżki, `if … then … else`, `in`, zakresy `[a..b]`). Reszta do listy. Rozpoznawanie po `language` lub prefiksie `=`.
- UI: okno importu z listą diagnostyk pogrupowaną po poziomach i tabelą tłumaczeń. „Importuj jako szkic” zapisuje definicję z `Unbound` i otwiera w edytorze z podświetleniem elementów do poprawy.
- **Zmienne `Static_*`** (§9 pkt 4 planu): domyślnie importujemy nazwy zmiennych 1:1 bez specjalnej semantyki (decyzja 5 w §8).

### Testy
- Tabela złotych przypadków JUEL → CEL oraz test różnicowy (wyrażenie źródłowe oceniane prostym interpreterem referencyjnym kontra CEL na tych samych zmiennych dla przypadków bez koercji).
- Każda konstrukcja z tabeli ma fixture XML z oczekiwaną diagnostyką.
- Fuzz/hostile: 512 znaków, głęboki nawias, nieznane przestrzenie nazw, `xsi:type` obcy.
- Idempotencja: import → eksport → import ścisłym parserem.
- Próbki NextApp po otrzymaniu.

### Ryzyka
Zakres JUEL łatwo rośnie. Trzymać się listy i ostrzeżeń. Błędny tłumacz jest groźniejszy niż jawna odmowa, bo cicho zmienia warunki bramek. Tłumaczenie uruchamiamy tylko ze zgodą użytkownika w oknie importu.

## 7. Pozycja 6: symulacja poza profilem `script_user_manual`

### Stan
Dopuszczone: skrypt, użytkownik, ręczne, bramki XOR/AND/OR, `TimerCatch` (`simulation.rs:2250-2316`). Wszystko inne to `SimulationUnsupportedActivity` (`simulation.rs:1212-1246`). `validate_supported_plan` (`simulation.rs:2391-2465`) odrzuca plany z jobami, wiadomościami, subskrypcjami, scope'ami, wywołaniami, powtórzeniami, błędami.

### Zasady
- Domyślnie symulacja **nie wywołuje** agentów, narzędzi ani bloków: zadanie usługi czeka na wynik podany przez operatora lub przygotowany z góry. Operator może per krok jawnie wybrać prawdziwe wykonanie (decyzja 6 właściciela): UI oznacza je jako mające skutki uboczne i koszty, a zapisy stanu procesu nadal trafiają wyłącznie do prywatnej bazy symulacji. Wykonanie idzie przez te same wykonawcy co produkcja, z tożsamością i autoryzacją operatora.
- Schemat bazy symulacji jest prywatny, więc nie potrzeba migracji produkcyjnej. Lista profili w `simulation_schema.rs:40` rośnie. Szablon schematu jest wersjonowany stałą w kodzie.
- Determinizm: alokator identyfikatorów (`SimulationIdSource`, `validate_plan_ids`) musi objąć nowe rodzaje identyfikatorów (scope'y, grupy i wystąpienia powtórzeń, subskrypcje, wiadomości, sygnały, wywołania).

### Pakiety (kolejno)
1. **S1: usługi i weryfikacja.** Nowa para `SimulationServiceResolveRequest/Response {simulation_id, node_id, result: ActivityResult}` opcjonalne `service_fixtures` w `SimulationStartRequest` (pole `#[serde(default)]`) oraz per krok wybór trybu `Result | Fixture | RealExecution` (domyślnie `Result`/`Fixture`; `RealExecution` wymaga jawnego potwierdzenia skutków ubocznych i kosztów). Zadanie usługi staje w stanie „czeka na wynik”, a UI pokazuje formularz wyniku (Completed/Error(kod)/NeedsHuman/Cancelled + wyjścia JSON) lub bierze wynik z fixtury. Przejścia po wyniku (weryfikacja `Human`/`Condition`, zdarzenia brzegowe błędu i eskalacji, `business_error`, `terminal_error`) stosuje ten sam planner co produkcja. Weryfikacje `Test` i `Critic` symulujemy wyborem werdyktu przez operatora.
2. **S2: wiadomości, sygnały, zdarzenia brzegowe.** Para żądań do wysłania wiadomości/sygnału w symulacji z kluczem korelacji. Subskrypcje i wyścigi bramki zdarzeń dostają zegar symulacji. Timery brzegowe i cykliczne. Pozostaje `validate_supported_plan` z węższą listą.
3. **S3: podproces, wywołanie procesu, powtórzenia, dodatkowe procesy.** Sceny (`create_scopes`), wywołania (opublikowane wersje przypięte do bazy symulacji przy starcie, `LocalBody` z tego samego dokumentu), grupy powtórzeń z limitami. Największy kawałek, bo dotyka najwięcej kodu plannera.
- Limity zasobów zostają (16 / 4 na organizację / 4 na właściciela, 16 MiB, TTL 30 min). Dodatkowo limit liczby scope'ów i wystąpień na symulację.
- UI: `process-simulation.js` dostaje panel „czeka na wynik” oraz wybór fixtur. Klucze i18n w 5 językach.
- Testy: scenariusze deterministyczne (te same identyfikatory i ślad przy ponowieniu), porównanie śladu symulacji z produkcyjnym wykonaniem tej samej definicji na przygotowanych wynikach (test zgodności), odrzucenie planów z dispatchem zewnętrznym.

### Ryzyko
`apply_simulation_plan_on` ma dowód `PrivateSimulationMode` i sprawdza prywatną bazę. Nowe zapisy muszą przejść te same kontrole. S3 wymaga ostrożnej pracy nad tymi ścieżkami i dobrych testów izolacji (żaden zapis do bazy produkcyjnej).

## 8. Podpakiety, kolejność i zależności

Szacunki dotyczą jednej osoby i przeglądu. Każdy pakiet ma być wdrażalny i wypychalny niezależnie, w następującej kolejności.

| Pakiet | Zakres | Zależy od | Czas | Migracja |
|---|---|---|---|---|
| **B3.1** Fundament + narzędzie addonu — **wdrożone 2026-10-13** (`95f3ee727`..`87c462d3b`) | `ServiceImplementation{Flow,AddonTool}`, walidacja, rozszerzenie pinu, rozgałęzienie `execute_claimed`, `call_tool_process`, `FlowOrigin::Process`, kodek JS, XML; odkładanie zajętych/niedostępnych narzędzi, odzyskiwanie wyników per wiersz z incydentami | brak | 3 dni | 201, 202 |
| **B3.2** Blok Flow Buildera — **wdrożone 2026-10-13** (`6308bf3b0`) | wariant `Block`, lista dozwolonych, graf syntetyczny, `dispatch_pinned_synthetic` | B3.1 | 2 dni | brak |
| **B3.3a** Opcje + paleta „Bloki TentaFlow” — **wdrożone 2026-10-15** (bez agentów, patrz „Wdrożone w B3.3a”) | rozszerzenie `OptionsResponse` (`service_blocks`, `service_tools`), szablony dynamiczne, drop, i18n, test e2e dla flow/blok/narzędzie | B3.1 (B3.2 dla bloków) | 2 dni | brak |
| **B3.3b** Inspektor z zakładkami + „Wynik i błędy” — **wdrożone 2026-10-17** (patrz „Wdrożone w B3.3b”) | `tf-tabs`, `processResultRouting`, ostrzeżenia, przyciski obsługi, i18n | B3.1 (równolegle z B3.3a) | 2-3 dni | brak |
| **B3.4a** Agent: ujście i wiązanie | `core.task_complete`, `bpmn_agent_completions`, zawężenie narzędzi na przebieg, `spawn_with_run_id` publiczny, walidacja | B3.1 | 3 dni | 203 |
| **B3.4b** Agent: wykonawca i odzyskiwanie | `execute_agent`, klasa slotów, adopcja ujścia po restarcie, `Contract` dla zdarzeń brzegowych, paleta agentów | B3.4a, B3.3a | 3 dni | brak |
| **B3.5** Weryfikacja „test” i „krytyk” | warianty `ActivityVerification`, etap weryfikacji w jobie, wiązanie `role=verification`, UI trybów | B3.2, B3.4b, B3.3b | 3 dni | brak (do potwierdzenia na prototypie) |
| **B3.6a** Tłumacz JUEL/FEEL → CEL | `expression_translate.rs`, testy złote i różnicowe | brak (w pełni równolegle) | 2 dni | brak |
| **B3.6b** Import Camundy | tryb tolerancyjny, mapowanie konstrukcji, `Unbound`, `translations`, okno importu | B3.6a, B3.1 | 3 dni | brak |
| **B3.7a** Symulacja S1 (usługi, weryfikacja) | nowe żądania, fixtury, panel UI | B3.1 (pełna po B3.5) | 3 dni | brak |
| **B3.7b** Symulacja S2 (wiadomości, sygnały, brzegowe) | jak wyżej | B3.7a | 3 dni | brak |
| **B3.7c** Symulacja S3 (podproces, wywołanie, powtórzenia) | jak wyżej | B3.7b | 3 dni | brak |
| **B3.8** Dokumentacja | aktualizacja `docs/BPMN_PROCESS_PLAN.md` §10 i `CLAUDE.md` (sekcja o procesach) po faktycznym wdrożeniu | wszystkie | 0,5 dnia | brak |

**Ścieżki równoległe:** (B3.1 → B3.2 → B3.3a/B3.3b → B3.4a → B3.4b → B3.5 → B3.7a) oraz niezależnie B3.6a → B3.6b. Drugi wykonawca może prowadzić ścieżkę importu bez kolizji, jeśli B3.1 jest już scalone (potrzebny `Unbound`).

Numery migracji 201 i 202 (B3.1) oraz 203 (B3.4a) są **rezerwacją na dziś**; drabina jest ciągła, więc numer przydziela się dopiero przy scaleniu. Rezerwacja TentaBus B7 przesunęła się na kolejny wolny numer. Sprawdzić kolizje z `origin/main` w dniu implementacji. Wszystkie testy przypinające numery (`run_ladder_up_to`, asercje `MAX(version)`, `call_pin_tests.rs`) zaktualizować w tym samym pakiecie.

## 9. Testy: zestawienie przekrojowe

- **Bajty zgodności:** testy złote dla modeli, zdarzeń `service_claimed` / `service_result` i `ProcessPayload` bez nowych pól (wzorzec testów w `processes.rs:2051`). Ten sam test dla `PinnedServiceSnapshot`.
- **Wykonanie:** każda implementacja ma testy ze ścieżek `jobs.rs` (cofnięcie dostępu w trakcie, anulowanie, wygaśnięcie dzierżawy, wynik po zamknięciu aktywności, restart).
- **Bezpieczeństwo:** próby przekierowania ujścia, wstrzyknięcie w danych procesu, obejście listy bloków, wyciek między organizacjami (użytkownik z org B publikuje definicję wskazującą narzędzie/agenta org A).
- **JS:** `bpmn.test.js`, `process-wire.test.js` oraz nowe testy palety i inspektora. Parytet kluczy i18n w 5 językach.
- **e2e:** `tests/e2e/process-b3-*.spec.js` (nowe). Pokrycie: palety i upuszczania, inspektora, importu Camundy, symulacji. Agentów i narzędzi nie testujemy na prawdziwym modelu, tylko skryptowym runnerem w testach Rust.
- Zgodnie z regułami repo: uruchamiane i raportowane tylko te testy, które faktycznie przeszły.

## 10. Ryzyka przekrojowe

**Znane ograniczenia B3.2:**
- Okno po sprawdzeniu przed uruchomieniem (ACL modelu odebrany w trakcie przebiegu) opisano w sekcji 3.2; wynik, który już istnieje, nie jest wtedy unieważniany przez sprawdzenie źródła przy akceptacji. Zaakceptowane bez zmian.
- Do osobnego pakietu (razem z „odmowa limitu jest tylko logowana dla zwykłych przepływów” i „zwykłe embeddingi omijają bramę AI”): `node_id` bramy AI to nazwa hosta, a dzierżawy tokenów są kluczowane identyfikatorem węzła mesh (`local_node_id`). To wcześniejszy wzorzec, obecny też w `agents/service.rs:236`; nie zmieniamy go w B3.2.

**Znane ograniczenia B3.1 (istniały wcześniej, bez zmian):** ponawianie wierszy z awarią infrastruktury skryptu odbywa się bez wycofania; konflikty rewizji przy starcie na wierszach przepływów są ciche (kolejny start ponawia).

1. **Cienka warstwa wokół ogrodzonego plannera.** Wariant A weryfikacji i adopcja ujścia po restarcie to jedyne miejsca, gdzie zmieniamy semantykę. Reszta jest dopisywaniem wykonawców. Każde odstępstwo od „co najwyżej raz” musi mieć test odtwarzania po awarii.
2. **Zgodność bajtowa modeli.** Najczęstsza pułapka: nowe pole bez `skip_serializing_if` zmienia SHA starych modeli i migracja 198 odrzuci start.
3. **Agent żyje poza transakcją procesu.** Zgodność przebiegu agenta z fazą wywołania opiera się na wiązaniu serwera i wierszu ujścia. Wyścigi (ujście po anulowaniu, podwójne ujście) muszą być obsłużone w jednym miejscu repozytorium.
4. **WASM bez przerywania.** Tool-task po timeoucie zostawia wątek. Limit równoległych wywołań narzędzi per addon (istniejąca pula instancji) może być wąskim gardłem.
5. **Głodzenie slotów.** Joby agentów muszą być osobną klasą w `runtime.rs:8301`, inaczej 4 długie zadania zablokują procesy.
6. **Węzły mesh.** Instancja i jej joby działają na jednym węźle, więc agent/narzędzie/blok muszą istnieć lokalnie. Przy przejęciu instancji przez inny węzeł zadania mogą zakończyć się odmową.
7. **Rozmiar prac w UI.** `config.js` i `bpmn.test.js` są duże. Rozbicie inspektora na zakładki wymaga ostrożnej aktualizacji testów selektorów.
8. **Tłumaczenie wyrażeń.** Cicha zmiana semantyki warunku w imporcie jest gorsza niż odmowa. Lista ostrzeżeń i zgoda użytkownika są obowiązkowe.
9. **Symulacja S3** dotyka najwięcej kodu plannera i izolacji zapisu. Rozbicie na S1/S2/S3 daje punkty odcięcia.
10. **Integracja z Projektami (poza B3).** Zwracamy uwagę tylko na wpływ projektu B3:
    - `FlowOrigin::Process` i wiązanie `process_activity` mają się nadawać do przyszłych procesów zadań projektu.
    - Narzędzia `core.project_*` wymagają powiązania z projektem. Nie rozstrzygamy tego w B3, wiązanie `process_activity` zostawia na to miejsce.
    - Równoległa bramka „Tester + Agent testujący + Agent security” jest osiągalna już po B3.4b/B3.5 (trzy zadania usługi + bramka złączająca).
    - Weryfikacja „test” przez `test-runner` Project Studio to przyszły blok/narzędzie, nie część B3.

## 11. Pytania wymagające decyzji właściciela

Sformułowane po ludzku, z moją rekomendacją.

1. **Którzy agenci mogą pracować w procesach?** Zadanie agenta w procesie może zużywać czas, tokeny i wywoływać narzędzia w imieniu osoby, która proces uruchomiła. Czy każdy, kto edytuje procesy, może wskazać dowolnego agenta, czy tylko tych, których administrator wcześniej zatwierdził?
   *Rekomendacja:* lista agentów zatwierdzonych przez administratora (domyślnie pusta, administrator włącza po jednym), bez zmian w tabeli agentów (ustawienie systemowe).
2. **Co jeśli agent zostanie zmieniony po opublikowaniu procesu?** Proces może używać od tej chwili jego nowej wersji (wygodne, ale zachowanie się zmienia bez nowej publikacji), albo zatrzymać się z błędem i czekać na ponowną publikację (bezpieczne, ale uciążliwe).
   *Rekomendacja:* używamy nowej wersji agenta, ale każda zmiana jest zapisana w historii instancji, a lista narzędzi dozwolonych dla kroku jest zapisana w samym procesie i nie rośnie, nawet gdy agent dostanie nowe narzędzia.
3. **Co ma się stać, gdy test lub krytyk odrzuci wynik?** Albo krok kończy się błędem, który można obsłużyć w procesie (np. ponowić pracę, wysłać wiadomość), albo zawsze trafia do człowieka. Do tego domyślny sposób sprawdzania wyniku agenta (otwarta decyzja nr 2 w planie).
   *Rekomendacja:* domyślnie „błąd do obsłużenia” z możliwością wyboru „do człowieka” per krok. Domyślne sprawdzanie: warunek na danych, jeśli agent ma zdefiniowany schemat wyniku, w przeciwnym razie człowiek. Krytyk nie może być tym samym agentem, który wykonał pracę.
4. **Limity agenta w tej fazie.** Czy wystarczy limit czasu i liczby kroków agenta oraz limit jednoczesnych zadań agentów (domyślnie 2 naraz w systemie i 3 na organizację), a limit kosztu/tokenów dodamy w osobnym kroku?
   *Rekomendacja:* tak. Koszt jest dziś widoczny w analityce, a twarde limity tokenów to osobna zmiana w silniku agentów.
5. **Import z Camundy: ile ufamy zaimportowanemu procesowi?** Importowane kroki „usługowe” (klasy Javy, zewnętrzne zadania) nie mają odpowiednika. Czy mają zostać jako puste kroki „do przypisania”, które trzeba ręcznie podłączyć do agenta, narzędzia lub przepływu przed publikacją (proces nigdy nie uruchomi się z niedokończonym krokiem)? Czy zmienne o nazwach `Static_*` z NextApp mają zachować nazwy bez specjalnego traktowania? Czy możemy dostać 2-3 prawdziwe pliki z NextApp jako próbki?
   *Rekomendacja:* tak, puste kroki „do przypisania” z blokadą publikacji. Nazwy zmiennych zostają 1:1, bez specjalnej logiki. Prosimy o próbki, bo od nich zależy zakres tłumaczenia wyrażeń.
6. **Symulacja a prawdziwe wywołania.** Czy symulacja ma zawsze pytać użytkownika o wynik kroku usługowego (lub używać przygotowanych wyników), a nigdy nie uruchamiać prawdziwych agentów i narzędzi?
   *Rekomendacja:* tak, nigdy nie uruchamia. Prawdziwe wykonanie to osobne uruchomienie instancji.

Otwarta decyzja nr 3 z planu (dostęp addonów do warstwy procesów) nie wchodzi do B3 i pozostaje bez zmian. Decyzja nr 1 (dwie warstwy) jest faktycznie podjęta przez wdrożenie B1/B2.

### Decyzje właściciela 2026-10-12

Rozstrzygają pytania z §11 i zastępują ich rekomendacje. Wdrażane są kolejnymi podpakietami, B3.1 ich nie dotyka poza numeracją migracji.

1. **Agenci w procesach:** DOWOLNY agent może być krokiem procesu. Brak listy zatwierdzonych przez administratora; obowiązują zwykłe reguły dostępu agenta i inicjatora.
2. **Zmiana agenta po publikacji:** proces używa nowej wersji agenta. Zmiana jest zapisana w historii instancji, a lista narzędzi dozwolonych dla kroku jest ustalona w procesie i nie rośnie.
3. **Odrzucenie przez test/krytyka:** domyślnie błąd do obsłużenia w procesie, per krok opcja „do człowieka”. Krytyk musi być innym agentem niż wykonawca pracy.
4. **Limity agenta:** BRAK dodatkowych limitów (ani kosztu, ani jednoczesności systemowej/na organizację). Zostaje wyłącznie wewnętrzna klasa szeregowania, żeby długie joby agentów nie zajęły wszystkich 4 slotów pętli runtime.
5. **Import z Camundy:** kroki usługowe bez odpowiednika stają się pustymi krokami „do przypisania” (`ServiceImplementation::Unbound`), a publikacja jest zablokowana do czasu przypisania implementacji. Nazwy zmiennych, w tym `Static_*`, zostają 1:1 bez specjalnej logiki.
6. **Symulacja a prawdziwe wywołania:** domyślnie symulacja pyta o wynik kroku usługowego albo bierze przygotowaną fixturę. Operator MOŻE opcjonalnie uruchomić dany krok naprawdę, wybierając to osobno dla każdego kroku. Takie wykonanie musi być jawne, w UI oznaczone jako mające skutki uboczne i koszty, a baza produkcyjna procesów pozostaje odizolowana (zapisy symulacji nadal tylko w prywatnej bazie). Zastępuje to zasadę „symulacja nigdy nie wywołuje” z §7 i rekomendację pytania 6.
7. **Numeracja migracji:** B3.1 = 201 i 202, B3.4a = 203; rezerwacja TentaBus B7 przesuwa się na kolejny wolny numer.

## 12. Najważniejsze pliki

- Plan i protokół:
  - `/Users/critix/repos/rust/TentaFlow/docs/BPMN_PROCESS_PLAN.md`
  - `/Users/critix/repos/rust/TentaFlow/tentaflow-protocol/src/processes.rs`
- Model, runtime, wykonanie:
  - `/Users/critix/repos/rust/TentaFlow/tentaflow-core/src/processes/model.rs`
  - `/Users/critix/repos/rust/TentaFlow/tentaflow-core/src/processes/jobs.rs`
  - `/Users/critix/repos/rust/TentaFlow/tentaflow-core/src/processes/runtime.rs`
  - `/Users/critix/repos/rust/TentaFlow/tentaflow-core/src/processes/repository.rs`
  - `/Users/critix/repos/rust/TentaFlow/tentaflow-core/src/processes/bpmn.rs`
  - `/Users/critix/repos/rust/TentaFlow/tentaflow-core/src/processes/simulation.rs`
  - `/Users/critix/repos/rust/TentaFlow/tentaflow-core/src/processes/simulation_schema.rs`
- Dispatch, przepływy, agenci, addony:
  - `/Users/critix/repos/rust/TentaFlow/tentaflow-core/src/dispatch/processes.rs`
  - `/Users/critix/repos/rust/TentaFlow/tentaflow-core/src/flow_engine/dispatcher.rs`
  - `/Users/critix/repos/rust/TentaFlow/tentaflow-core/src/flow_engine/node_adapters/activity_result.rs`
  - `/Users/critix/repos/rust/TentaFlow/tentaflow-core/src/flow_engine/node_adapters/tool_exec.rs`
  - `/Users/critix/repos/rust/TentaFlow/tentaflow-core/src/agents/run_manager.rs`
  - `/Users/critix/repos/rust/TentaFlow/tentaflow-core/src/agents/builtins.rs`
  - `/Users/critix/repos/rust/TentaFlow/tentaflow-core/src/agents/principal.rs`
  - `/Users/critix/repos/rust/TentaFlow/tentaflow-core/src/agents/service.rs`
  - `/Users/critix/repos/rust/TentaFlow/tentaflow-core/src/project_studio/generation.rs`
  - `/Users/critix/repos/rust/TentaFlow/tentaflow-core/src/addon/mod.rs`
  - `/Users/critix/repos/rust/TentaFlow/tentaflow-core/src/db/migrations.rs`
- UI:
  - `/Users/critix/repos/rust/TentaFlow/tentaflow-core/www/js/modules/flows-builder/bpmn.js`
  - `/Users/critix/repos/rust/TentaFlow/tentaflow-core/www/js/modules/flows-builder/palette.js`
  - `/Users/critix/repos/rust/TentaFlow/tentaflow-core/www/js/modules/flows-builder/config.js`
  - `/Users/critix/repos/rust/TentaFlow/tentaflow-core/www/js/modules/flows-builder/process-simulation.js`
  - `/Users/critix/repos/rust/TentaFlow/tentaflow-core/www/js/protocol/codec.js`
  - `/Users/critix/repos/rust/TentaFlow/tentaflow-core/www/i18n/pl.json` (i pozostałe cztery języki)
- Powiązany plan Projektów (tylko wpływ B3): `/Users/critix/repos/rust/TentaFlow/docs/PROJECT_STUDIO_WORKFLOW_PLAN.md` §3.2.
