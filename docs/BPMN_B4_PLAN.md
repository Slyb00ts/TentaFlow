# Plan techniczny BPMN, faza B4 (monitor: nakładka tokenów, incydenty z ponowieniem i powiadomieniem, migracja instancji)

Stan wyjściowy: koniec B3 (2026-11-05), `origin/main` na `72685d186`. Plan wynika z lektury kodu, nie z założeń: wszystko, co istnieje, opisano w §0, a to, czego brak, jest dopiero treścią pakietów. Źródła wymagań: `docs/BPMN_PROCESS_PLAN.md` §6 (Monitor procesów), §10.2a (decyzja o „Ponów dla zablokowanego mapowania wyjścia” i lista tego, co musi dodać B4) oraz `docs/BPMN_B3_PLAN.md` (zakładka Czas bez ponowień, ostrzeżenie importu `failedJobRetryTimeCycle`).

Plan nie zmienia kodu. Numery migracji są rezerwacją na dziś (§9).

## 0. Najważniejsze ustalenia z kodu (wpływ na projekt)

**Co już jest (nie budować drugi raz).**

- **Okno instancji i lista** (`www/js/modules/flows-builder/process-monitor.js`: `openProcessInstances`, `openProcessInstance`): lista z filtrem tylko po definicji, okno instancji z sekcjami zadań, incydentów, timerów, wywołań, zakresów, powtórzeń, subskrypcji, wyścigów zdarzeń, wiadomości wychodzących, zmiennych i historii. Odświeżanie to `setInterval(refresh, 3000)` z numerem generacji odczytu i porównaniem migawki; nie ma kanału push dla procesów.
- **Protokół** (`tentaflow-protocol/src/processes.rs`, `ProcessPayload`): `InstanceList/Get/Cancel`, `JobRetry`, `History`, `ScopeGet`, `UserTaskGet/Complete`, `Message*`, `Simulation*`. `ProcessInstance` niesie `active_node_ids` (płaska lista `DISTINCT node_id` ze stanów `ready/waiting/joining`, bez zakresu i bez stanu tokenu), `incidents` z `can_retry`, `scopes`, `calls`, `repetition_*`, a `ProcessInstancePageRequest` stronicuje kolekcje. **Nie ma** żadnego wariantu migracji instancji, listy incydentów ponad instancjami ani ponowienia innego niż zadanie usługi.
- **Ponowienie zadania usługi** (`repository::retry_job` + `runtime::plan_service_retry`): tylko inicjator (`instance.initiator_user_id == actor.user_id`), tylko instancja `Incident` z zgodną rewizją, tylko wywołanie w fazie `proved_no_effect` z dowodem `no_boundary` i zadanie w stanie `error` (`job_direct_retryable_on`). Plan konsumuje stary token, tworzy nowy token `ready` (nowy `token_id`), zamyka incydent i zapisuje zdarzenie `job_retried`; wpis `AcceptedInputRef::Retry` jest ogrodzeniem. Lista instancji liczy `can_retry` tym samym warunkiem SQL.
- **Odroczenie bez incydentu** (`repository::defer_job`, `DEFER_DEADLINE_MS` = 10 min, `TOOL_BUSY`/`TOOL_UNAVAILABLE`): jedyny istniejący mechanizm „ponów po czasie”, ale tylko dla zajętych narzędzi, bez konfiguracji w modelu.
- **Kody incydentów**: ~70 kodów (`ACTIVITY_IO_OUTPUT_FAILED`, `ACTIVITY_IO_INPUT_FAILED`, `SCRIPT_MAPPING_FAILED`, `SCRIPT_EVALUATION_FAILED`, `EXTERNAL_OUTCOME_UNCERTAIN`, `EXTERNAL_RESULT_NOT_APPLIED`, `VERIFICATION_FAILED`, `NO_MATCHING_FLOW`, `AMBIGUOUS_GATEWAY`, `TIMER_ERROR`, `SOURCE_ACCESS_REVOKED`, limity `*_LIMIT`, …). Tabela `bpmn_incidents(incident_id, instance_id, scope_id, node_id, job_id, code, message, at_ms, resolved_at_ms)` nie ma pola „rodzaj ponowienia” ani licznika prób.
- **Dane do nakładki istnieją w bazie**: `bpmn_tokens(token_id, instance_id, scope_id, node_id, arrival_edge_id, status ∈ ready|waiting|joining|consumed|cancelled)`; tokeny skonsumowane nie są usuwane, więc `arrival_edge_id` skonsumowanych tokenów jest zapisem przebytych krawędzi. Zdarzenia `bpmn_events(seq, kind, node_id, data_json)` niosą `sequence_flow_id` (np. `exclusive_selected`) i `selected_branch_edge_ids`. `ScopeGet` zwraca `active_node_ids` per zakres.
- **Diagram nie jest bpmn-js.** Edytor to własne płótno `FlowCanvas` (`flows-builder/canvas.js`) w trybie `mode: 'bpmn'`, z parametrem `readOnly` i odwzorowaniem modelu `processToCanvas`/`canvasToProcess` (`bpmn.js`). Węzły są elementami DOM z `data-node-id`, istnieje już mechanizm znaczników uwagi (`attentionIds`, `attentionContainerIds`, znaczek ostrzeżenia na zadaniu). Wersja opublikowana z diagramem (`ProcessVersion.model.diagram`, `ProcessShape`/`ProcessEdgeDiagram`) przychodzi z `VersionGetRequest`. Okno instancji **nie pokazuje diagramu w ogóle**.
- **Dostęp**: instancję widzi inicjator i osoba z przypisanym zadaniem (`require_instance_reader`), lista filtruje to samo (`i.initiator_user_id=?2 OR assignee`). Właściciel definicji (`owner_user_id`) i administrator **nie widzą cudzych instancji**. To wprost blokuje „powiadomienie właściciela procesu” z §6 planu: właściciel nie mógłby otworzyć incydentu, o którym go powiadomiono.
  *Stan po B4.1:* właściciel definicji i posiadacz `processes.monitor` (migracja 207) czytają teraz także cudze instancje (`ProcessInstanceAccess`: `Initiator`, `Owner`, `Assignee`, `Monitor`); monitor widzi nazwy i typy zmiennych, nie wartości, klucze korelacji subskrypcji ani treść błędów (tylko kod). Ponowienie działa w imieniu inicjatora i wymaga, by inicjator nadal posiadał definicję (to samo sprawdza pobranie zadania przez workera), więc właściciel inny niż inicjator widzi `can_retry=false`.
- **Powiadomienia**: jedyna infrastruktura to Project Studio: tabela `notifications` w `projects.db` (kolumna `project_id`), `project_studio::notifications::notify` (dedup nieprzeczytanego tego samego `user+kind+link`, best-effort) i zdarzenie `SystemEventPayload::UserNotification` (prywatne per użytkownik, filtrowane w `ws_binary.rs`). Odbiornik zdarzenia jest w `project-studio.js`; globalny odbiornik zdarzeń systemowych (`modules/system-events.js`) obsługuje tylko `ServiceStatusChanged` i `MeshPeerStatusChanged`. Dla BPMN nie ma skrzynki ani dzwonka.
- **Audyt**: `audit_log` z łańcuchem skrótów; repozytorium procesów zapisuje akcje `process.*`; historia instancji w `bpmn_events`.
- **Import Camundy** (`processes/bpmn/camunda.rs`): `failedJobRetryTimeCycle` daje ostrzeżenie `CAMUNDA_RETRY_CYCLE_SKIPPED` („per-activity retries do not exist yet”). Zakładka Czas w inspektorze ma tylko `timeoutSeconds` (`ProcessNodeKind::ServiceTask.timeout_seconds`).
- **Migracje i wersja protokołu**: ostatnia migracja w kodzie to **206** (`bus_field_policy_mask_hash`, TentaBus F4 B7); wszystkie 183-205 to BPMN. `SCHEMA_VERSION` = 32 (`tentaflow-protocol/src/envelope.rs`); nowe warianty `ProcessPayload` dopisuje się na końcu (tagi po NAZWIE), nowe pola z `#[serde(default)]`, a w modelu z `skip_serializing_if`, żeby nie zmieniać SHA starych modeli (inaczej migracja 198 odrzuci start).

**Czego brakuje, a wpływa na projekt.**

1. Nakładka potrzebuje tokenów **z zakresem i stanem**, nie płaskiej listy węzłów (powtórzenia i podprocesy dają kilka tokenów na tym samym węźle w różnych zakresach).
2. Dowody wykonania są przypięte do wersji: `bpmn_activity_io_witnesses` ma niemutowalne `definition_id, version, model_sha256, body_path_json, io_spec_sha256` (wyzwalacz `bpmn_activity_io_witnesses_cas`), `bpmn_timers`, `bpmn_repetition_groups`, `bpmn_calls`, `bpmn_call_pins` mają `definition_id`+`version`, a zadania i wywołania usług są przypięte do `service_snapshots_json` wersji. **Migracja instancji nie może po prostu zmienić `bpmn_instances.version`**: musi wiedzieć, które dowody wolno przepiąć, a które zostają historią.
3. „Ponów dla zablokowanego mapowania wyjścia” jest świadomie odłożone do B4 z konkretną listą (§10.2a): kolumna generacji w kluczu `uq_bpmn_io_activation`, kolejny wiersz świadka po `output_blocked`, `AcceptedInputRef::OutputRetry`, gałąź walidatora, projekcja `can_retry` dla `ACTIVITY_IO_OUTPUT_FAILED` i `SCRIPT_MAPPING_FAILED` oraz decyzja, względem jakich zmiennych liczyć.

## 1. Projekt wspólny

### 1.1 Zasady przekrojowe

- **Rozszerzenie, nie fork.** Nowe żądania to nowe warianty `ProcessPayload` na końcu enuma. Istniejące `ProcessIncident`, `ProcessInstance`, `ProcessInstanceSummary` rosną o pola z `#[serde(default)]`; testy złote bajtów bez nowych pól (wzorzec z B3) pozostają zielone.
- **Jedna brama odczytu.** `require_instance_reader` dostaje trzeci powód dostępu: właściciel definicji (i posiadacz uprawnienia `processes.monitor`, decyzja D1). Funkcja zwraca dziś `bool` „inicjator”; zamieniamy to na typ `InstanceAccess { Initiator, Assignee, Owner, Monitor }` z metodami `may_retry()` i `may_migrate()`. Każdy konsument (`instance_on`, `incident_on`, `list_instances`, `scope_get_on`) dostaje zmianę w jednym miejscu, nie w kilkunastu.
- **Wykonanie zawsze w imieniu inicjatora.** Ponowienie przez właściciela nie zmienia, czyje uprawnienia wykonują pracę: zadania nadal biegną z uprawnieniami inicjatora (`require_call_control_authority_on` wymaga `initiator == actor`; dla ponowienia przez właściciela potrzebny jest wariant sprawdzający inicjatora z bazy, nie z aktora). Właściciel może więc tylko ponowić to, co inicjator i tak zlecił, na przypiętych wersjach. To jest świadome ograniczenie eskalacji (D1).
- **Polityka UI.** Tylko komponenty `tf-*` (`tf-window`, `tf-table`, `tf-tabs`, `tf-segmented`, `tf-chip`, `tf-button`, `tf-select`, `tf-toggle`, `tf-tooltip`, `tf-alert`), zero surowych `<button>/<input>`. W UI słowo „node”, nie „węzeł” (decyzja właściciela), dotyczy m.in. węzłów mesh; elementy diagramu BPMN nazywamy „element” (zgodnie z `element_unavailable`). Klucze i18n w `bpmn.*` identyczne w pl/en/de/es/fr (parytet kluczy to prosty test liczbowy).
- **Polskie liczby mnogie** przez `{count|forma1|forma2|forma3}`, nigdy konkatenacją (np. „3 incydenty”).
- **Bez ścieżek kompatybilności**: stare ścieżki zastępujemy w miejscu (np. `can_retry` liczone w SQL listy zastępujemy jedną projekcją).

### 1.2 Model dowodów a wersja (kluczowe dla pozycji 2 i 3)

Dowód wykonania ma trzy klasy, i ta klasyfikacja steruje zarówno ponowieniem mapowania, jak i migracją:

| Klasa | Przykłady | Zachowanie przy migracji |
|---|---|---|
| **Historia** (zamknięta, nie wpływa na dalsze wykonanie) | `bpmn_events`, zakończone zadania, zaakceptowane świadki, zamknięte incydenty, skonsumowane tokeny | Nie ruszamy. Zachowują stare `version`/`model_sha256`. UI rozwiązuje nazwy elementów po wersji zdarzenia (§4.4). |
| **Żywe, przepinalne** (czeka, nie ma jeszcze skutku zewnętrznego) | token `waiting` na zadaniu użytkownika, timer `pending`, subskrypcja wiadomości, wyścig zdarzeń, zadanie usługi w `error` + `proved_no_effect` | Przepinamy `node_id`/`version` tylko gdy odcisk elementu jest równy (§4.2). |
| **Żywe, nieprzepinalne** (skutek mógł zajść lub trwa) | zadanie `running`, wywołanie `may_have_executed`/`uncertain`/`observed`, otwarty świadek IO `input_captured`/`result_accepted`, żywe wywołanie procesu, otwarta grupa powtórzeń o innej konfiguracji | Migracja odmawia z listą powodów. |

## 2. Pozycja 1: nakładka tokenów na diagramie instancji

### Stan

Okno instancji ma tylko listy; diagram jest w edytorze, a okno wersji (`openProcessCallPreview`) pokazuje podgląd wywołania. Dane: `active_node_ids` (płaskie), `ProcessInstance.scopes`, `ScopeGet`. Brak widoku „przebyta ścieżka”.

### Decyzja o źródle danych

Nie rozbudowujemy `ProcessInstance` o tokeny (jest już u granicy budżetu ramki 960 KiB i ma dziewięć stronicowanych kolekcji). Nowe, osobne żądanie tylko do odczytu:

```
InstanceTokensGetRequest  { instance_id, scope_id: Option<String>, if_revision: Option<u64> }
InstanceTokensGetResponse { instance_revision, not_modified: bool, version, process_id,
                            tokens: Vec<ProcessTokenView>, taken_edge_ids: Vec<String>,
                            visited_node_ids: Vec<String>, truncated: bool }
ProcessTokenView { token_id, scope_id, node_id, status, wait: Option<ProcessTokenWait>,
                   incident_ids: Vec<String>, occurrence: Option<{group_id, ordinal}>, since_ms }
ProcessTokenWait  = UserTask{user_task_id} | Job{job_id} | Timer{timer_id, due_at_ms}
                  | Message{subscription_id} | Call{child_instance_id} | Gateway | Scope{scope_id}
```

- `if_revision`: gdy rewizja instancji się nie zmieniła, odpowiedź to `not_modified` bez ciała (tani odczyt co 2-3 s; rewizja rośnie przy każdej zmianie planu).
- Tokeny aktywne (`ready/waiting/joining`) zawsze; skonsumowane tylko do wyznaczenia `taken_edge_ids`/`visited_node_ids` (z `arrival_edge_id` i zdarzeń `exclusive_selected`/`selected_branch_edge_ids`), z limitem (np. 2000 krawędzi, `truncated=true` ponad limit, nigdy ciche ucięcie).
- Zakres: bez `scope_id` zwracamy zakres główny plus zbiorcze liczniki dla podprocesów (`ProcessTokenView` z `wait=Scope`); z `scope_id` tokeny tego zakresu. To odpowiada drążeniu w podproces na płótnie (`processPath`).
- Wywołania procesu: dziecko ma własny diagram, więc nakładka pokazuje token `Call{child_instance_id}` z przyciskiem „Otwórz instancję” (istnieje `call_open_child`), bez rysowania diagramu dziecka w oknie rodzica (D-poza zakresem).

### Odświeżanie

Odpytywanie z `if_revision` w tej samej pętli co dotychczasowe `refresh` (3 s), z numerem generacji odczytu i anulowaniem po zamknięciu okna (istniejący wzorzec). Push przez `SystemEventPayload` odrzucamy w B4: zdarzenia systemowe są broadcastem bez filtra instancji, a ciężar odpytywania z `not_modified` jest znikomy. Ponowne rozważenie, gdy będzie wiele otwartych monitorów (§8, ryzyko 5).

### Renderowanie

- Nowa sekcja okna instancji (przełącznik `tf-segmented`: „Lista” / „Diagram”; wybór zapamiętany w `localStorage` w `try/catch`) tworząca `FlowCanvas({ mode: 'bpmn', readOnly: true })` z modelem wersji instancji (`VersionGetRequest`, cache per `definitionId:version`, bo wersje są niemutowalne).
- Nowe API płótna `setRuntimeOverlay({ tokens, takenEdgeIds, visitedNodeIds })` obok `attentionIds` (ten sam mechanizm klas CSS na `[data-node-id]`; krawędzie dostają klasę przez istniejące identyfikatory SVG): klasy `fb-node--token-active`, `--token-waiting`, `--token-incident`, `--visited`, `fb-edge--taken`. Licznik tokenów na elemencie (`×3`) jako element badge (precedens `fb-node-badge`), dla elementów-kontenerów (podproces, wywołanie) zbiorczy licznik z potomków (precedens `attentionContainerIds`). Kolory z tokenów CSS (`--tf-*`), stan nie może polegać wyłącznie na kolorze (ikona + `aria-label`), animacja pulsu tylko bez `prefers-reduced-motion`.
- Kliknięcie elementu otwiera panel boczny w oknie: tokeny elementu, powód oczekiwania (`wait`), powiązany incydent z przyciskami z pozycji 2, link do zadania/zadania usługi/timera. Zero nowej logiki: panel używa istniejących renderów sekcji.
- Powtórzenia: element z grupą powtórzeń pokazuje „aktywne 2 z 5” z `ProcessRepetitionGroupSummary`; szczegół ordinalny w istniejącym panelu `repeat_*`.
- Wersja zmigrowana: diagram ładowany z bieżącej `instance.version`; po migracji (pozycja 3) odświeżenie pobiera nowy model.

### Testy

Rust: projekcja tokenów dla przypadków: równoległe rozgałęzienie, podproces, powtórzenie sekwencyjne i równoległe, kilka tokenów na tym samym elemencie w różnych zakresach, instancja po migracji, `not_modified`, limit krawędzi. JS: czysty moduł `process-token-overlay.js` (mapowanie odpowiedzi na klasy) z testem jednostkowym. e2e `process-b4-overlay.spec.js` (§7).

## 3. Pozycja 2: incydenty, ponawianie, powiadomienia

### 3.1 Semantyka ponowienia według rodzaju incydentu

Ponowienie to nie jeden przycisk. Rodzaj ponowienia jest właściwością incydentu liczoną po stronie serwera (`ProcessIncident.retry: ProcessIncidentRetry`, `#[serde(default)]`; `can_retry` zostaje jako pochodna `retry.kind != None`), a UI nigdy nie zgaduje po kodzie.

| Rodzaj (`retry.kind`) | Incydenty | Co robi ponowienie | Warunek idempotencji |
|---|---|---|---|
| **`Job`** (jest) | błędy zadania usługi z `proved_no_effect` + `no_boundary` | Istniejący `retry_job`: nowy token, nowe zadanie, nowa próba | Dowód braku skutku w wierszu wywołania |
| **`JobUncertain`** (nowe, wymuszone) | `EXTERNAL_OUTCOME_UNCERTAIN`, `EXTERNAL_RESULT_NOT_APPLIED` (wywołanie `uncertain`/`may_have_executed`) | Nowe wywołanie, ten sam `stable_request_id` (ten sam logiczny żądanie, narzędzie może deduplikować), zdarzenie `job_retried_uncertain` z ryzykiem duplikatu | Nie ma dowodu; wymaga jawnego potwierdzenia „rozumiem ryzyko podwójnego wykonania” i tylko właściciel definicji (D2). Do zweryfikowania na prototypie: unikalność `stable_request_id` w `bpmn_service_invocations` i zachowanie hostów narzędzi |
| **`OutputMapping`** (nowe) | `ACTIVITY_IO_OUTPUT_FAILED`, `SCRIPT_MAPPING_FAILED` | Ponowna ewaluacja skojarzeń wyjścia na **tym samym zaakceptowanym wyniku** (praca nie jest wykonywana ponownie) | Pozycja 2.3 |
| **`None`** | `SOURCE_ACCESS_REVOKED`, limity (`*_LIMIT`), `IMMEDIATE_TRANSITION_LIMIT`, `ACTIVITY_IO_INPUT_FAILED`, `NO_MATCHING_FLOW`, `AMBIGUOUS_GATEWAY`, `*_EXPRESSION_ERROR`, `VERIFICATION_FAILED` (jest zadaniem człowieka), `TIMER_ERROR`, `STORE_BUSY` itd. | Brak przycisku. Ponowienie deterministycznego błędu wyrażenia na tych samych danych daje ten sam błąd | Wyjście z incydentu: anulowanie instancji albo migracja do wersji z poprawką (pozycja 3) |

Każdy incydent `None` dostaje w odpowiedzi `retry.denied_reason` (kod, tłumaczony w UI), żeby okno mówiło „dlaczego nie” zamiast ukrywać przycisk. Wyjątek świadomie poza B4: ponowienie zdarzeń czasowych (`TIMER_ERROR`, timery mają własne `next_check_at_ms`) i bramek, patrz §6.

### 3.2 Kto może ponawiać (RBAC)

- Czytanie: inicjator, osoba z przypisanym zadaniem (jest), **właściciel definicji** i posiadacz `processes.monitor` (nowe, domyślnie `org_admin`; migracja 207 dopisuje uprawnienie jak migracja 119 dla Projektów).
- Ponowienie `Job`/`OutputMapping`: inicjator lub właściciel definicji. Ponowienie `JobUncertain` i migracja: tylko właściciel definicji (zmienia semantykę „co najwyżej raz” i model pod inicjatorem).
- Zawsze sprawdzane na serwerze w transakcji (`expected_revision`, `command_id`, powtórzenie komendy odtwarza wynik): odmowa to jednolite „nie znaleziono instancji” dla osoby bez dostępu do odczytu (brak wycieku istnienia), a „brak uprawnienia do operacji” dla czytającego bez prawa operacji.
- Wykonanie nadal w imieniu inicjatora; jeśli inicjator stracił prawo do przypiętego źródła, ponowienie kończy się istniejącym `SOURCE_ACCESS_REVOKED` zamiast eskalacji (`require_source_current`).

### 3.3 Ponowienie mapowania wyjścia (generacja)

Powody odłożenia z §10.2a rozwiązujemy zgodnie z tamtą listą:

- **Migracja 209**: kolumna `generation INTEGER NOT NULL DEFAULT 0` w `bpmn_activity_io_witnesses`; indeks `uq_bpmn_io_activation` przebudowany jako `(instance_id, scope_id, token_id, phase_owner, COALESCE(occurrence_id,''), generation)`. Wyzwalacze `…_initial_phase` i `…_cas` aktualizowane (generacja niezmienna w wierszu, początkowy wiersz generacji N+1 startuje fazą dozwoloną dla ponowienia). Przebudowa tabeli wg wzorca migracji 185 (kopia, ponowne utworzenie indeksów i wyzwalaczy, test drabiny).
- **Nowy wiersz świadka** po `output_blocked`: generacja+1, faza początkowa `result_accepted` (wynik pochodzi z poprzedniej generacji; `result_event_id` wskazuje to samo zdarzenie, więc unikalny indeks `uq_bpmn_io_result_event` wymaga zmiany na `(result_event_id, generation)` albo wiersz ponowienia niesie `retried_from_witness_id`; wybór po prototypie, jedna z dwóch dróg bez osłabiania gwarancji unikalności zdarzenia).
- `AcceptedInputRef::OutputRetry { witness_id, generation, command_id, request_hash, expected_instance_revision }`, gałąź walidatora planu ponowienia w `repository.rs` (obok gałęzi `Retry`): dozwolone tylko przy otwartym incydencie `ACTIVITY_IO_OUTPUT_FAILED`/`SCRIPT_MAPPING_FAILED`, tokenie nadal `waiting` i niezmienionym zaakceptowanym wyniku (porównanie bajtowe z wierszem wywołania).
- **Względem jakich zmiennych**: wynik pracy jest przypięty (bez ponownego wykonania), a skojarzenia wyjścia liczone są względem **bieżących zmiennych zakresu** (D3). To nowa semantyka, więc UI mówi wprost: „Wynik pracy zostanie zastosowany ponownie do bieżących zmiennych”. Jeśli cała przyczyna to niezmienne wyrażenie, ponowienie zwróci ten sam błąd, a nowa generacja zostawi wiersz dowodowy (nie pętla: ponowienie zużywa generację i zapisuje incydent; limit 5 generacji na aktywację, potem `None` z `denied_reason=OUTPUT_RETRY_LIMIT`).
- Wyjście z niezmiennego błędu mapowania to migracja do wersji z poprawionym skojarzeniem (pozycja 3); to wymaga, by generacja N+1 mogła nieść nowe `version`/`io_spec_sha256`. Zapis tego rebindingu jest poza v1 migracji (§4.5), ale kolumna generacji go umożliwia bez kolejnej przebudowy.

### 3.4 Polityka ponowień per aktywność (`failedJobRetryTimeCycle`)

- **Model**: nowe pole `retry: Option<ProcessRetryPolicy>` na `ProcessNodeKind::ServiceTask` (`skip_serializing_if = "Option::is_none"`, `#[serde(default)]`; test bajtów złotych starego modelu). `ProcessRetryPolicy { max_attempts: u8 (1..=10), delay_seconds: u32 (1..=86400), backoff: Fixed | Exponential{factor_percent, max_delay_seconds} }`. Domyślnie brak pola = 0 ponowień (zachowanie dzisiejsze). Wyłącznie zadania usługi w B4 (skrypt, wysyłka, wiadomości mają inne dowody), z wyłączeniem kroku agenta o wysokim koszcie jeśli właściciel zdecyduje inaczej (D6).
- **Co jest ponawiane automatycznie**: wyłącznie awarie z dowodem braku skutku (`proved_no_effect`), czyli dokładnie klasa `Job` z §3.1. `EXTERNAL_OUTCOME_UNCERTAIN` nigdy automatycznie. Wyczerpanie prób zostawia zwykły incydent `retry.kind=Job`, z licznikiem w komunikacie („po 3 próbach”).
- **Wykonanie**: stan prób przy wywołaniu (migracja 210: `attempt_index` i `next_attempt_at_ms` na `bpmn_jobs` albo w `bpmn_service_invocations`; wybór po lekturze `defer_job`, który już ma przechowywane odroczenie i klucz `DEFER_*`), pętla czasu runtime ponawia przez ten sam `plan_service_retry` z `AcceptedInputRef::Retry` (aktor systemowy jako inicjator zlecenia, bez eskalacji), zdarzenie `job_retry_scheduled` z numerem próby i terminem. Wspólna ścieżka z ręcznym `retry_job`, żeby nie powstały dwie semantyki ponowienia.
- **Walidacja przy publikacji**: wartości w granicach, `timeout_seconds * max_attempts` w limicie życia zadania, polityka na elemencie bez `Flow/Block/AddonTool/Agent` (np. `Unbound`) odrzucona.
- **Inspektor**: zakładka „Czas” zmienia nazwę na „Czas i ponowienia” (klucz i18n w pięciu językach), pola `tf-input type=number` + `tf-select` z trybem. Brak ukrytych domyślnych wartości; podpowiedź mówi, że ponawiane są tylko awarie bez skutku.
- **Import Camundy**: `failedJobRetryTimeCycle` (`R3/PT10M`, `R5/PT1M,PT5M,PT15M`) mapowany: `R<n>` → `max_attempts = n`, jeden interwał → `Fixed`, lista interwałów → najbliższe `Exponential` albo ostrzeżenie `CAMUNDA_RETRY_CYCLE_APPROXIMATED` z oryginałem; interwał spoza zakresu lub zapis w niewspieranym formacie nadal `CAMUNDA_RETRY_CYCLE_SKIPPED` (kod zostaje dla przypadków, których nie da się odwzorować). Zmiana tekstu ostrzeżenia „per-activity retries do not exist yet” w kodzie, klucz diagnostyki w UI bez zmiany nazwy.
- **Symulacja**: nie symuluje prób; kroki z polityką wykonują jeden raz (fixture może zwrócić błąd), a okno symulacji wyświetla informację „polityka ponowień pominięta w symulacji”.

### 3.5 Powiadomienia o incydentach

- **Kto**: inicjator instancji i właściciel definicji (osoby różne, bez duplikatu, gdy to ta sama osoba). Zawsze osoby aktywne w organizacji instancji; dezaktywowany odbiorca jest pomijany bez błędu.
- **Kiedy**: przy utworzeniu incydentu (nie przy każdym odświeżeniu); incydenty tła (powtarzane skanem odzyskiwania) nie wysyłają ponownie. Reguła bezpieczeństwa: powiadomienie nie niesie danych procesu, tylko nazwę procesu, element, kod i identyfikatory (analogicznie do zasady „logi bez treści” w Meeting Bot).
- **Dedup**: tabela `bpmn_incident_notices(incident_id, user_id, notified_at_ms, PRIMARY KEY(incident_id, user_id))` (migracja 208). Zapis w tej samej transakcji, w której powstaje incydent (`INSERT OR IGNORE`), wysyłka po commicie; nieudana wysyłka nie wycofuje incydentu (best-effort, jak `project_studio::notifications::notify`). Po restarcie nie wysyłamy ponownie, bo wiersz istnieje.
- **Kanał**: `SystemEventPayload::UserNotification { user_id, notification_id, project_id: "", kind: "process_incident", title, body, link_json }` (istniejący prywatny kanał per użytkownik, nowy wariant enuma niepotrzebny). `link_json` = `{ "instanceId": "…", "incidentId": "…" }`. Globalny odbiornik w `modules/system-events.js` (dziś nie obsługuje `UserNotification`) pokazuje `tf-toast` z akcją „Otwórz instancję” prowadzącą do `openProcessInstance`. Odbiornik w `project-studio.js` ignoruje `kind` spoza swojej listy (do sprawdzenia w testach, żeby jego toast nie dublował).
- **Trwałość**: bez osobnej skrzynki. Trwałym odpowiednikiem jest widok „Wymagają uwagi” w liście instancji (§4.1) filtrujący po otwartych incydentach, dla mnie i dla procesów, których jestem właścicielem; liczba w nagłówku listy. Dzwonek Project Studio pozostaje tylko dla Projektów (D4).
- **Brak kanałów zewnętrznych** (e-mail, webhook, TentaBus) w B4. Opcja: publikacja incydentu na temat TentaBus jako osobny, późniejszy pakiet.

## 4. Pozycja 3: migracja instancji do nowej wersji definicji

### 4.1 Zasada

Camunda Cockpit pozwala przenieść token na dowolną aktywność; tu nie, bo dowody wykonania są przypięte do wersji i do bajtów modelu (§1.2). Migracja B4 to **zgodna strukturalnie zmiana wersji instancji**: tokeny zostają tam, gdzie są, a mapa przepina je na odpowiedniki o równym odcisku. Dalsza część procesu (wszystko, czego tokeny jeszcze nie dotknęły) może się dowolnie różnić. To pokrywa zasadniczy przypadek użycia: poprawiłem proces dalej w dół, chcę, by działające instancje poszły nową ścieżką.

### 4.2 Reguły (plan migracji = czysta funkcja nad migawką)

Wejście: `instance_id`, `target_version`, mapa `node_mapping: Vec<{from_scope_path, from_node_id, to_node_id}>` (domyślnie tożsamościowa, jawna tylko dla zmian identyfikatorów), `expected_revision`.

Odmowy twarde (kody tłumaczone w UI, wszystkie zwracane razem, nie pierwszy z brzegu):

1. Ta sama definicja i organizacja; wersja docelowa opublikowana, niezarchiwizowana, **większa** od bieżącej (cofanie wersji poza B4); definicja niearchiwizowana.
2. Instancja w stanie `running/waiting/incident`; nie `completed/cancelled`. Instancja `running` z gotowym tokenem: odmowa do czasu przetworzenia (migracja działa tylko na stanie spoczynku: brak tokenów `ready`, brak zadań `running/claimed`).
3. Ten sam wybrany proces (`selected_process_id`) i start (`start_node_id`) istnieją w wersji docelowej; ścieżka ciał zakresów (`body_path`) istnieje.
4. **Odcisk elementu** dla każdego elementu z żywym stanem (token, timer, subskrypcja, wyścig, zadanie użytkownika, grupa powtórzeń, zakres) musi być równy w wersji docelowej po mapowaniu. Odcisk to hasz kanoniczny: rodzaj elementu + wszystko, co przypięty dowód porównuje (`io_spec_sha256`, implementacja zadania usługi i jej pin, weryfikacja, wyrażenie wyniku, reguła timera i przypięty kalendarz, nazwa wiadomości i klucz korelacji, specyfikacja przypisania i pól zadania użytkownika, konfiguracja powtarzania, identyfikatory zdarzeń brzegowych przypiętych do hosta). **Wyjątek**: zadanie usługi w `error` + `proved_no_effect` albo bez zadania (czeka na ponowienie) może zmienić implementację i skojarzenia wejścia/wyjścia, o ile rodzaj elementu pozostaje `ServiceTask`; po migracji ponowienie (ręczne lub z polityki) utworzy zadanie z przypięć wersji docelowej. To jest główna ścieżka „napraw proces → zmigruj → ponów”.
5. Zbiór zdarzeń brzegowych na elemencie z żywym tokenem identyczny (dodanie ani usunięcie granicy nie jest rozbrojone/uzbrojone wstecz w B4).
6. Zero żywych wywołań procesu (`bpmn_calls` otwarte) w żadnym kierunku: ani instancja rodzic, ani dziecko; przypięcia wywołań (`bpmn_call_pins`) są per wersja i wymagałyby przepięcia zależności.
7. Brak wywołań w fazach nieprzepinalnych i świadków IO w fazach `input_captured`/`result_accepted` (§1.2).
8. Inicjator ma prawo wykonać wersję docelową (`require_version_execution_on` dla inicjatora) i dostęp do przypiętych źródeł docelowych elementów, do których dojdą tokeny w pierwszej kolejności (ostrzeżenie, nie odmowa: dalsze kroki mogą być odmówione dopiero w trakcie).
9. Rozmiary: do 200 żywych tokenów i 200 wierszy dowodowych w jednej migracji; powyżej odmowa `MIGRATION_TOO_LARGE` (bez częściowego zastosowania).

Ostrzeżenia (nie blokują, widoczne w podglądzie): usunięte zadeklarowane zmienne procesu; zmienione skojarzenia zadań, do których tokeny jeszcze nie dotarły; element docelowy `Unbound`; nowa wersja ma inną strefę czasową timerów niż stara.

### 4.3 Co się przepina

W jednej transakcji `IMMEDIATE` z CAS rewizji (nowa metoda `repository::migrate_instance` obok `retry_job`, ten sam szkielet komendy z `command_id`):

- `bpmn_instances.version` (+ `model` odczytywany z nowej wersji; FK `(definition_id, version)` wymusza istnienie wersji), `selected_process_id` bez zmiany.
- `bpmn_tokens.node_id` (tylko żywe), `bpmn_timers.version/node_id` (`pending`; wiersz `fired/cancelled` to historia), `bpmn_event_subscriptions`, `bpmn_event_races`, `bpmn_user_tasks.node_id`, `bpmn_repetition_groups.version/node_id` oraz zależne wiersze wystąpień, `bpmn_scopes` (podproces: `subprocess_node_id`).
- Nic z historii: zdarzenia, zamknięte świadki IO, zaakceptowane wywołania, skonsumowane tokeny.
- Nowy wiersz `bpmn_instance_migrations` (migracja 211): `migration_id, instance_id, from_version, to_version, plan_sha256, mapping_json, actor_user_id, at_ms, revision_before, revision_after`; plus zdarzenie `instance_migrated` w `bpmn_events` (z mapą i hashem planu) oraz wpis `audit_log` `process.instance_migrated` (łańcuch skrótów). Zdarzenia zostają ze starymi identyfikatorami, więc UI historii rozwiązuje nazwy elementów po wersji obowiązującej w chwili zdarzenia (z listy migracji instancji); nieznany element → istniejące `element_unavailable`.
- Kandydaci do zbadania na etapie B4.7 (prototyp przed kodem): wszystkie tabele z kolumną `node_id` lub `version`/`definition_id` (~79 wystąpień `node_id TEXT` w migracjach). Wynik: tabela „przepina / historia / odmowa” w kodzie jako jedna lista `MIGRATION_TABLES`, a **test kompletności** (każda tabela `bpmn_*` z `node_id` albo `version` musi być w tej liście; dodanie nowej tabeli bez wpisu wywala test). To zabezpieczenie przed cichym pominięciem tabeli w przyszłych fazach (B5).

### 4.4 Podgląd (dry-run) i zastosowanie

- `InstanceMigrationPlanRequest { instance_id, target_version, node_mapping }` → `InstanceMigrationPlanResponse { plan_sha256, instance_revision, can_apply, blockers: Vec<{code, element_id, scope_id, message_key, params}>, warnings, mapped_tokens: Vec<{from, to, kind, will_be_repointed}>, unmapped_elements, source_model_sha256, target_model_sha256 }`. Czysty odczyt (`read_snapshot`), zero zapisów, bez efektów ubocznych.
- `InstanceMigrateRequest { command_id, instance_id, target_version, node_mapping, expected_revision, plan_sha256 }` → `InstanceMigrateResponse { instance }`. Zastosowanie **liczy plan od nowa w transakcji** i porównuje `plan_sha256`; różnica to odmowa „stan instancji zmienił się od podglądu” (nie stosujemy planu z pamięci). Po commicie `runtime::wake`.
- Dopuszczone tylko dla właściciela definicji (D1). Operacja idempotentna po `command_id` (powtórzenie zwraca zapisany wynik).
- Podgląd i zastosowanie dla jednej instancji. Lista „migruj wiele” w UI jako pętla żądań z podglądem każdej i tabelą wyników, limit 50 (UI, bez atomowości między instancjami; D5).

### 4.5 Poza zakresem (jawnie)

- Przenoszenie tokenu na inny, nierównoważny element (Camunda „modify instance”), dodawanie i usuwanie tokenów.
- Cofanie do starszej wersji.
- Migracja instancji z żywym wywołaniem procesu (rodzic i dziecko), z zadaniem `running`, z otwartym świadkiem IO ani wywołaniem w fazie `may_have_executed/uncertain/observed`.
- Przepinanie świadka IO do nowej wersji (generacja z B4.5 to umożliwia, ale operacja jest osobna).
- Dodanie lub usunięcie zdarzenia brzegowego aktywnego elementu.
- Migracja wsadowa po stronie serwera, edycja zmiennych instancji, ponowienie bramek i błędów wyrażeń.
- Migracja symulacji.

## 5. Pozycja 4: UI monitora (wspólna dla trzech)

- **Lista instancji**: tabela `tf-table` z filtrami (`tf-toolbar`): definicja, wersja, stan, „tylko z incydentem”, „moje / jestem właścicielem”, wyszukiwanie po nazwie. `InstanceListRequest` rośnie o pola opcjonalne `version`, `status`, `has_incident`, `scope: Mine|Owned|Monitor` (`#[serde(default)]`); filtr „zmienna” z §6 planu poza B4 (zmienne nie są indeksowane, wymagałoby osobnej decyzji). Zmiana starego `can_retry` w SQL listy na tę samą projekcję co w widoku instancji.
- **Okno instancji**: nagłówek z „Migruj do wersji…”, przycisk w sekcji incydentów zależny od `retry.kind` (`Ponów`, `Ponów mimo niepewności` z `tf-window.confirm` z polem potwierdzenia, `Zastosuj wynik ponownie`), przy `None` powód z `denied_reason`.
- **Okno migracji**: `tf-window` w dwóch krokach (wybór wersji docelowej `tf-select` + mapa elementów jako `tf-table` z `tf-select` na wiersz; podgląd z listą powodów odmowy i ostrzeżeń, przycisk „Zastosuj” aktywny tylko przy `can_apply`). Diagram źródłowy i docelowy obok siebie (dwa `FlowCanvas` read-only) z podświetleniem elementów z tokenami; to ten sam moduł nakładki.
- Każdy tekst przez `I18n.t('bpmn.*')`, parytet kluczy w pięciu językach i test liczbowy.

## 6. Poza zakresem B4 (jawnie)

- Ponowienie błędów wyrażeń, bramek i timerów (`TIMER_ERROR`), edycja zmiennych instancji, filtr po zmiennej, wsad serwerowy migracji.
- Powiadomienia e-mail/webhook/TentaBus, skrzynka powiadomień poza Projektami.
- Rysowanie diagramu instancji dziecka (wywołanie) w oknie rodzica.
- Push zamiast odpytywania.
- Elementy B5 (podprocesy zdarzeniowe, kompensacja, zdarzenia warunkowe, bramka złożona, transakcje); migracja ich nie obsługuje i musi odmawiać dla instancji z takimi stanami, gdy się pojawią (test kompletności z §4.3 to wymusi).
- `RealExecution` symulacji (decyzja właściciela 2026-11-05: odrzucone).

## 7. Testy: strategia

- **Zgodność bajtowa**: testy złote `ProcessPayload` (nowe warianty dopisane na końcu, stare tagi bez zmian), `ProcessIncident`/`ProcessInstance` bez nowych pól dekodują się starym kodem, SHA starych modeli bez `retry` bez zmian (wzorzec z `processes.rs`).
- **Migracje**: drabina `run_ladder_up_to`, asercje `MAX(version)`, `call_pin_tests.rs` aktualizowane w tym samym pakiecie; dla 209 test przebudowy indeksu na bazie z istniejącymi świadkami w każdej fazie; dla 211 FK i CHECK.
- **Rust, ponawianie**: macierz kodów incydentów → `retry.kind` (jeden test sprawdza, że każdy kod z listy ma jawny wpis, więc nowy kod bez klasyfikacji wywala test); ponowienie po restarcie; wyścig dwóch ponowień (jedno wygrywa, drugie `revision conflict`); `OutputRetry` po zmianie zaakceptowanego wyniku odrzucone; limit generacji; polityka ponowień: wyczerpanie prób, brak automatu dla `uncertain`, restart w trakcie oczekiwania na kolejną próbę; powiadomienia: dedup po restarcie, brak wysyłki przy powtórnym skanie.
- **Rust, migracja**: każda reguła odmowy ma test, każdy przypadek przepięcia ma test z kompletnością tabel (§4.3); migracja w trakcie wyścigu z timerem i wiadomością; idempotentny `command_id`; zmiana `plan_sha256` między podglądem a zastosowaniem; zmigrowana instancja kończy proces nową ścieżką; zdarzenia i audyt.
- **Bezpieczeństwo**: właściciel bez dostępu nie widzi cudzej organizacji; osoba bez prawa nie odróżnia „brak” od „zabronione”; ponowienie przez właściciela wykonuje się z uprawnieniami inicjatora; powiadomienie nie niesie danych procesu.
- **JS**: `process-token-overlay.test.js`, testy żądań w `codec.js`/`requests.js`, parytet kluczy i18n.
- **e2e (Playwright, `tests/e2e/process-b4-*.spec.js`, uruchamiane po stronie właściciela, nie na tej maszynie współdzielonej w trakcie planowania)**: nakładka (aktywne, oczekujące, incydent, przebyta ścieżka, drążenie w podproces), ponowienie z listy, migracja z podglądem i odmową, toast powiadomienia (login z wymuszoną rotacją hasła, patrz notatki o e2e). Zgodnie z regułami repo raportujemy tylko to, co faktycznie uruchomiono.

## 8. Ryzyka

1. **Migracja jest największym ryzykiem fazy.** Zbyt szeroka wymusza zmianę niemutowalnych dowodów; zbyt wąska będzie bezużyteczna. Zabezpieczenia: reguły z §4.2 odmawiają w razie wątpliwości, test kompletności tabel (§4.3), plan liczony od nowa przy zastosowaniu, brak migracji wsadowej w serwerze.
2. **Przebudowa indeksu `uq_bpmn_io_activation` (migracja 209)** dotyka tabeli z wyzwalaczami dowodowymi. Wymaga przebudowy tabeli i testu na realistycznych danych; błąd tu psuje zapis IO każdej aktywności. Pakiet B4.5 pozostaje osobny, żeby go wycofać bez reszty fazy.
3. **`JobUncertain` łamie „co najwyżej raz”.** Dozwolony tylko jawnie, tylko właścicielowi, z zapisem ryzyka w zdarzeniu. Hosty narzędzi muszą otrzymać ten sam `stable_request_id`; nie wszystkie to wykorzystają. Prototyp przed kodem (§3.1).
4. **Uprawnienia właściciela i `processes.monitor`** poszerzają krąg osób widzących zmienne instancji (mogą zawierać dane osobowe). Zmienne w widoku monitora dla roli `Monitor` redagujemy do nazw i typów, wartości tylko dla inicjatora i właściciela (D1); to musi być w kontrakcie `ProcessInstance` (`variables_redacted`).
5. **Odpytywanie wielu otwartych monitorów.** `not_modified` czyni zapytanie tanim, ale `read_snapshot` na każdej instancji co 3 s skaluje się liniowo z liczbą okien. Pomiar przed rozważaniem push.
6. **Zmiana `can_retry` w liście instancji** (jedno wyrażenie SQL zamiast kilkunastu połączeń) może zmienić wyniki starych testów; zmieniamy je w miejscu, nie obok.
7. **Nowy globalny odbiornik `UserNotification`** może zdublować toast Project Studio. Filtr po `kind` i test obu odbiorników jednocześnie.
8. **Rozrost `process-monitor.js`** (1733 linii). Nakładkę i okno migracji dodajemy jako osobne moduły (`process-token-overlay.js`, `process-migration.js`), nie do pliku monolitu.
9. **Numeracja migracji.** Rezerwacje TentaBus przesuwają numery; sprawdzić `origin/main` w dniu implementacji. Numer przydziela się przy scaleniu, nie z góry.

## 9. Pakiety, kolejność i zależności

Szacunki dla jednej osoby wraz z przeglądem. Każdy pakiet wdrażalny i wypychalny osobno. Numery migracji to rezerwacja od 207 (ostatnia w drzewie to 206); 207 zajęło uprawnienie `processes.monitor` z B4.1, więc dalsze pakiety są przesunięte o jeden (B4.3 → 208, B4.5 → 209, B4.6 → 210, B4.8 → 211).

| Pakiet | Zakres | Zależy od | Czas | Migracja |
|---|---|---|---|---|
| **B4.1** Czytnik monitora | `InstanceAccess` (właściciel, `processes.monitor`), rozszerzone filtry listy, jedna projekcja `can_retry`, redakcja zmiennych dla roli `Monitor`, `InstanceTokensGet` z `if_revision`, kodek JS, testy złote | brak | 2 dni | 207 |
| **B4.2** Nakładka tokenów | `process-token-overlay.js`, `FlowCanvas.setRuntimeOverlay`, sekcja „Diagram” w oknie instancji, panel elementu, drążenie w zakres, i18n w 5 językach, e2e | B4.1 | 3 dni | brak |
| **B4.3** Powiadomienia o incydentach | `bpmn_incident_notices`, emisja `UserNotification` po utworzeniu incydentu, odbiornik w `system-events.js`, widok „Wymagają uwagi” na liście, testy dedup | B4.1 | 2 dni | 208 |
| **B4.4** Klasyfikacja i ponawianie zadań | `ProcessIncidentRetry` (`kind`, `denied_reason`), test kompletności kodów, uprawnienia właściciela, `JobUncertain` (prototyp `stable_request_id`), UI przycisków i powodów odmowy | B4.1 | 2 dni | brak |
| **B4.5** Ponowienie mapowania wyjścia | generacja w `uq_bpmn_io_activation`, wiersz świadka po `output_blocked`, `AcceptedInputRef::OutputRetry`, gałąź walidatora, limit generacji, UI | B4.4 | 3 dni | 209 |
| **B4.6** Polityka ponowień per aktywność | `ProcessRetryPolicy` w modelu, walidacja publikacji, ponawianie automatyczne przez `plan_service_retry`, zakładka „Czas i ponowienia”, mapowanie `failedJobRetryTimeCycle`, symulacja pomija | B4.4 | 3 dni | 210 |
| **B4.7** Migracja: plan i podgląd | odcisk elementu, reguły §4.2, `MIGRATION_TABLES` i test kompletności, `InstanceMigrationPlan*`, okno podglądu (dwa diagramy obok siebie) | B4.1, B4.2 | 3 dni | brak |
| **B4.8** Migracja: zastosowanie i dokumentacja | `repository::migrate_instance`, `bpmn_instance_migrations`, zdarzenie, audyt, odtwarzanie nazw historii po wersji zdarzenia, e2e migracji, aktualizacja `BPMN_PROCESS_PLAN.md` §6/§10, `CLAUDE.md` | B4.7 (pełne zalety po B4.5) | 3 dni | 211 |

**Wymaganie powiadomień (B4.3) dla B4.4 i B4.6:** ponowienie, ręczne i automatyczne, nie może tworzyć nowego wiersza `bpmn_incidents` na każdą próbę. Jedna awaria aktywności to jeden incydent aż do rozwiązania; kolejne próby aktualizują licznik prób na tym incydencie. Trigger kolejki powiadomień (migracja 208) powiadamia o każdym nowym wierszu, więc incydent na próbę wysyłałby powiadomienie na próbę.

**Razem ok. 21 dni.** Ścieżki: (B4.1 → B4.2 → B4.7 → B4.8) oraz równolegle (B4.1 → B4.4 → B4.5/B4.6) oraz B4.3. Drugi wykonawca może prowadzić B4.3/B4.4 bez kolizji po scaleniu B4.1. B4.5 jest przed B4.8 wyłącznie dla spójnego opisu (generacja daje później rebinding świadka), nie jako twarda zależność kodu.

## 10. Decyzje właściciela (do rozstrzygnięcia przed B4.1)

1. **Kto widzi i obsługuje cudze instancje?** Dziś tylko inicjator i osoba z zadaniem; bez zmiany powiadomienie dla właściciela procesu prowadzi donikąd. *Rekomendacja:* właściciel definicji czyta i ponawia (w imieniu inicjatora), migruje i wymusza ponowienie niepewne tylko on; uprawnienie `processes.monitor` (domyślnie `org_admin`) daje czytanie bez mutacji, ze zmiennymi zredagowanymi do nazw i typów.
2. **Ponowienie po niepewnym skutku** (`EXTERNAL_OUTCOME_UNCERTAIN`): zakazać na stałe czy dopuścić jawnie? *Rekomendacja:* dopuścić tylko właścicielowi, z potwierdzeniem ryzyka podwójnego wykonania i tym samym `stable_request_id`; nigdy automatycznie, także z polityką ponowień.
3. **Ponowienie mapowania wyjścia liczone względem których zmiennych?** *Rekomendacja:* zaakceptowany wynik pracy przypięty, mapowanie względem bieżących zmiennych zakresu, limit 5 generacji; bez edycji zmiennych w B4 (naprawa błędnego mapowania to migracja do wersji z poprawką).
4. **Powiadomienia: jaki kanał i trwałość?** *Rekomendacja:* toast na żywo (prywatne `UserNotification`) do inicjatora i właściciela, dedup trwały, bez nowej skrzynki; trwałym śladem jest filtr „Wymagają uwagi” na liście instancji. E-mail/TentaBus jako osobny, późniejszy pakiet.
5. **Zakres migracji w v1:** ta sama definicja, tylko do wersji nowszej, tokeny zostają na równoważnych elementach (równy odcisk), bez żywych wywołań procesu, zadań `running` i otwartych świadków IO; migracja pojedynczej instancji z podglądem i `plan_sha256`. *Rekomendacja:* przyjąć; Camundowe „modify instance” (dowolne przeniesienie tokenu) odrzucić, bo łamie niemutowalne dowody.
6. **Domyślna polityka ponowień i które kroki ją dostają:** *Rekomendacja:* brak ponowień domyślnie (0), maksymalnie 10 prób, odstęp 1 s do 24 h, stały lub wykładniczy; automatycznie ponawiane tylko awarie z dowodem braku skutku; krok agenta dopuszczony, ale z podpowiedzią o koszcie w inspektorze. Zaimportowany `R<n>/PT…` mapowany na tę politykę, a niemapowalne cykle dalej ostrzeżeniem.

## 11. Najważniejsze pliki

- Plany: `docs/BPMN_PROCESS_PLAN.md` (§6, §10.2a, §10.3), `docs/BPMN_B3_PLAN.md`.
- Protokół: `tentaflow-protocol/src/processes.rs` (`ProcessPayload`, `ProcessIncident`, `ProcessInstance`, `ProcessInstancePageRequest`, `ProcessNodeKind::ServiceTask`).
- Rdzeń: `tentaflow-core/src/processes/repository.rs` (`retry_job`, `job_direct_retryable_on`, `require_instance_reader`, `instance_state_on`, `list_instances`, `defer_job`), `runtime.rs` (`plan_service_retry`, `AcceptedInputRef`), `jobs.rs`, `bpmn/camunda.rs` (`failedJobRetryTimeCycle`), `model.rs` (walidacja publikacji).
- Dispatch i migracje: `tentaflow-core/src/dispatch/processes.rs`, `tentaflow-core/src/db/migrations.rs` (ostatnia 206; `bpmn_activity_io_witnesses`, `uq_bpmn_io_activation`, `bpmn_incidents`, `bpmn_tokens`, `bpmn_timers`).
- Powiadomienia: `tentaflow-core/src/project_studio/notifications.rs` (wzorzec `notify`), `SystemEventPayload::UserNotification` (`message_body.rs`), `api/dashboard/ws_binary.rs` (filtr per użytkownik), `www/js/modules/system-events.js`.
- UI: `www/js/modules/flows-builder/process-monitor.js`, `canvas.js` (`attentionIds`, `readOnly`), `bpmn.js`, `config.js` (zakładka Czas), `www/js/protocol/codec.js` i `requests.js`, `www/i18n/{pl,en,de,es,fr}.json`.
