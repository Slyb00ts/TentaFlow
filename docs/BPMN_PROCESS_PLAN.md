# Procesy BPMN 2.0 we Flow Builderze — plan (warstwa procesów + bloki Flow Buildera)

> **Rewizja 0.1** (2026-09-29). Odpowiedź na wymaganie: **pełne wsparcie BPMN** we Flow Builderze
> i **łączenie klocków BPMN z blokami Flow Buildera** (w kroku procesu wywołać agenta, inny flow,
> blok addonu), w tym przypadek „agent wykonuje zadanie i zwraca tylko, czy się udało”.
>
> Punkt wyjścia: `docs/HARNESS_PLAN.md` §3.12 (zmienne i io-mapping, CEL), §3.13 (`ask_user` jako
> odpowiednik User Task), §6 (mapa drogowa BPMN). Konsumenci: workflow zadań w Projektach
> (`PROJECT_STUDIO_WORKFLOW_PLAN.md` §3), procesy bezpieczeństwa (PSIRT, obowiązki cykliczne),
> dowolne procesy firmowe (zatwierdzenia, onboarding). Pozycje **[do decyzji]** czekają na odpowiedź.

---

## 0. Decyzje (propozycja)

1. **Dwie warstwy, jeden edytor.** **Warstwa procesów** wykonuje BPMN (tokeny, czekanie dniami,
   ludzie, timery, wiadomości, zdarzenia brzegowe, bramki). **Silnik flow (`flow_engine`)** wykonuje
   obliczenia (LLM, agent, narzędzia, bloki addonów). Proces nie ma własnych bloków obliczeniowych:
   każde „zadanie usługi” deleguje pracę do bloku, przepływu albo agenta Flow Buildera. W edytorze
   to jedno płótno Flow Buildera z dwoma poziomami (§3).
2. **To jest świadoma korekta `HARNESS_PLAN.md` §6** („import BPMN kompilowany do grafu bloków, nie
   drugi silnik”). Uzasadnienie:
   - `flow_engine` to **wykonanie przepływu danych** — koperta przechodzi przez graf, przebieg jest
     ograniczony deadline'em i żyje w pamięci (`agents/run_manager.rs:17,329`),
   - BPMN to **semantyka tokenów sterowania** — elementów, które w pełni działają dopiero na
     tokenach, nie da się uczciwie przełożyć na graf danych:
     - przerywające zdarzenie brzegowe musi anulować trwającą aktywność,
     - bramka inkluzywna złącza tylko gałęzie, które faktycznie wystartowały,
     - bramka oparta na zdarzeniach wybiera „co przyjdzie pierwsze”,
     - podproces zdarzeniowy,
     - wielodniowe zadania ludzi.

   Obawa z §6 — „dwa silniki = podwójny koszt każdej zmiany bloków” — nie zachodzi, bo warstwa
   procesów **nie dubluje żadnego bloku**. Zna tylko sterowanie, a każdą pracę zleca silnikowi flow.
   Mapa z §6.1 (Exclusive Gateway = `condition`, Call Activity = `subflow` …) zostaje ważna
   **wewnątrz** przepływów obliczeniowych. **[do decyzji — kluczowa]**
3. **Kontrakt wyniku aktywności** (§4). Każda praca zlecona z procesu kończy się jednym ze
   standardowych wyników: `ukończone` / `błąd(kod)` / `wymaga człowieka(powód)` / `anulowane`, plus
   zmienne wyjściowe i dowody. Wynik mapuje się wprost na elementy BPMN (przepływ dalej, zdarzenie
   brzegowe błędu, zdarzenie brzegowe eskalacji). To odpowiada na „agent wykona zadanie i zwróci,
   czy się udało”.
4. **„Agent mówi, że zrobił” nie wystarcza tam, gdzie to ważne.** Zadanie agenta ma definicję
   ukończenia i tryb weryfikacji (§4.3). Domyślnie wynik agenta jest **sprawdzany** — warunkiem na
   danych, uruchomieniem testu albo agentem-krytykiem — a przy krokach ryzykownych dodatkowo
   przez człowieka.
5. **Jedna warstwa procesów dla całej platformy.** Workflow zadań w Projektach to **instancja procesu
   BPMN na każde zadanie**. To zastępuje w planie Projektów rozwiązanie przejściowe („stan w bazie
   projektu, flow tylko do kroków”, decyzja 3). Gdy warstwa procesów istnieje, nie budujemy drugiej
   maszyny stanów w Projektach.
6. **„Pełne BPMN” = wykonywalny podzbiór klasy Camunda 7** w pierwszych fazach, z jawną listą
   tego, czego jeszcze nie ma (§2). Specyfikacja BPMN 2.0 zawiera elementy, których nie wykonuje
   w praktyce żaden popularny silnik (np. choreografie). Uczciwa tabela poziomów jest lepsza niż
   obietnica „wszystkiego”.
7. **Import i eksport BPMN 2.0 XML**, także z rozszerzeniami `camunda:`. Procesy z NextApp
   (Camunda 7: `Process_RackChange_ExternalApproval.bpmn` i inne) dają się wczytać i porównać.

## 1. Dwa poziomy w jednym edytorze

```text
┌─ Flow Builder: PROCES (BPMN) ───────────────────────────────────────────────┐
│ tory = funkcje/grupy · zdarzenia · bramki · zadania użytkownika · timery     │
│                                                                              │
│  (Start) → [Zadanie użytkownika: Analiza] → <XOR> → [Zadanie usługi: ▣ ] → … │
│                                                          │                   │
│                                        implementacja: ───┘                   │
│                                        • blok Flow Buildera (np. LLM, tool)  │
│                                        • przepływ Flow Buildera (subflow)    │
│                                        • agent (z definicją ukończenia)      │
│                                        • narzędzie addonu                    │
└──────────────────────────────────────────────────────────────────────────────┘
        dwuklik na zadaniu usługi ▼
┌─ Flow Builder: PRZEPŁYW (jak dziś) — trigger → llm → tool_exec → output ─────┐
│ wejście = zmienne procesu (input mapping) · wyjście = wynik aktywności (§4)  │
└──────────────────────────────────────────────────────────────────────────────┘
```

- **Paleta procesu:** kategorie BPMN (Zdarzenia, Zadania, Bramki, Tory, Dane) oraz kategoria
  **„Bloki TentaFlow”**: wszystkie dotychczasowe bloki, agenci i przepływy. Upuszczenie bloku albo
  agenta na płótno procesu tworzy **zadanie usługi z tą implementacją** (zamiast pytać o typ), więc
  łączenie klocków BPMN z naszymi jest jednym ruchem.
- **Dwuklik na zadaniu usługi** otwiera jego przepływ na płótnie Flow Buildera (wewnętrzny albo
  wskazany istniejący). Breadcrumb na dole jest jak dziś: „PROCES › Implementacja › PRZEPŁYW”.
- **Inspektor** zadania usługi ma zakładki: Implementacja (blok / przepływ / agent / narzędzie) ·
  Wejścia i wyjścia (io-mapping CEL z §3.12) · Wynik i błędy (mapowanie wyników na zdarzenia
  brzegowe) · Ponowienia i czas · Zaawansowane (identyfikator BPMN, dokumentacja).

## 2. Zakres BPMN — poziomy

| Poziom | Elementy | Kiedy |
|---|---|---|
| **1 — procesy ludzi i automatów** | zdarzenia startu (zwykłe, czasowe, wiadomości), końca (zwykłe, błędu, terminate); zadania: użytkownika, usługi (blok/przepływ/agent/narzędzie), skryptu (CEL), wysłania i odbioru wiadomości, ręczne; bramki: wyłączna, równoległa, **inkluzywna**, **oparta na zdarzeniach**; zdarzenia pośrednie: czasowe, wiadomości (catch/throw), sygnał; **zdarzenia brzegowe**: czasowe (przerywające i nie), błędu, eskalacji, wiadomości; podproces osadzony; wywołanie procesu (call activity); wielokrotne wykonanie (równoległe/sekwencyjne); pętla; tory i pule; obiekty danych i adnotacje | fazy B1–B3 |
| **2 — procesy złożone** | podproces zdarzeniowy (przerywający i nie), zdarzenia warunkowe, zdarzenie linku, **kompensacja** (zdarzenie i obsługa), bramka złożona (complex), transakcja | faza B5 |
| **Poza zakresem** | choreografie, diagramy konwersacji, reguły biznesowe DMN jako osobny silnik (zamiast tego: zadanie skryptu CEL albo blok) | — |

Import dokumentu z elementem spoza obsługiwanego poziomu kończy się **czytelną listą** („zdarzenie
kompensacji w kroku X — obsługa w poziomie 2”), a nie cichym pominięciem.

## 3. Warstwa procesów — jak działa

- **Model wykonania:** instancja procesu, tokeny (wykonania) na elementach, zmienne w zakresach
  (proces / podproces / lokalne aktywności), subskrypcje (timery, wiadomości, sygnały),
  zadania pracy (joby), incydenty.
- **Trwałość:** każdy stan oczekiwania zapisywany w bazie (zadanie użytkownika, timer, oczekiwanie
  na wiadomość, oczekiwanie na wynik aktywności) — po restarcie instancje wracają dokładnie tam,
  gdzie były. Silnik flow nie musi dostać trwałych przebiegów, żeby proces czekał dniami, bo
  czekanie należy do warstwy procesów.
- **Zlecenia pracy (joby):** token wchodzi w zadanie usługi → powstaje job → wykonuje go silnik flow
  / `AgentRunManager` → wynik wraca po identyfikatorze joba. Ponowienia per aktywność (liczba,
  odstęp, wzrost). Po wyczerpaniu prób powstaje **incydent** widoczny w monitorze (§6), a nie cisza —
  lekcja z NextApp: incydenty Camundy, których nikt nie oglądał.
- **Idempotencja:** każde wykonanie niesie klucz `instancja · aktywność · próba`. Blok z efektem
  ubocznym (wysłanie maila, zgłoszenie w zewnętrznym systemie, commit) dostaje go i ma go użyć
  (wzór z NextApp: znacznik zamiaru przed POST-em w integracji SDP).
- **Czas:** timery w strefie procesu (kalendarz roboczy z Projektów jako opcja — „za 24 h pracy”).
- **Wiadomości i korelacja:** wiadomość przychodzi z nazwą i kluczem korelacji (np. zdarzenie git
  „MR scalony” z kluczem `NA-231`, odpowiedź klienta z numerem zgłoszenia). Warstwa procesów dopasowuje
  ją do subskrypcji instancji. Źródła: webhooki (Projekty §7), TentaBus, API, inne procesy.
- **Mesh:** instancją zarządza jeden węzeł naraz (dzierżawa z odnawianiem). Po utracie węzła
  dzierżawa wygasa i inny węzeł przejmuje instancję od ostatniego zapisanego stanu.

## 4. Kontrakt wyniku aktywności — „czy zadanie zostało wykonane”

### 4.1 Wynik

| Wynik | Znaczenie | BPMN |
|---|---|---|
| `ukończone` | praca wykonana i (jeśli skonfigurowano) zweryfikowana | przepływ sekwencji dalej |
| `błąd(kod, opis)` | praca się nie udała w przewidziany sposób (np. `TESTS_FAILED`, `NOT_REACHABLE`) | **zdarzenie brzegowe błędu** z tym kodem; bez złapania — incydent |
| `wymaga_człowieka(powód)` | wykonawca nie może rozstrzygnąć (brak danych, niejednoznaczność, niska pewność) | **zdarzenie brzegowe eskalacji** → zwykle zadanie użytkownika |
| `anulowane` | aktywność przerwana (np. przez przerywający timer) | obsługiwane przez warstwę procesów |
| przekroczony czas | job nie wrócił w terminie | **zdarzenie brzegowe czasowe** |

Do tego zawsze: **zmienne wyjściowe** (przez output mapping), **podsumowanie dla człowieka**,
**dowody** (odnośniki, załączniki, zrzuty, logi) i metryki (czas, tokeny, model).

### 4.2 Zadanie agenta

Konfiguracja w inspektorze:
- **agent** i jego model — z konfiguracji agenta, domyślnie lokalny,
- **cel** — szablon z CEL, np. „Popraw błąd `${task.key}`: `${task.title}`”,
- **dozwolone narzędzia** i **budżet** (czas, tokeny, liczba iteracji),
- **definicja ukończenia** — lista warunków po ludzku i/lub formalnie, np. „testy jednostkowe
  zielone”, „MR otwarty z kluczem zadania”,
- **schemat wyniku** (JSON Schema) — np. `{ "fixed": bool, "mr": string, "notes": string }`.

Wykonanie: agent działa w swojej pętli (HARNESS §3) i **musi zakończyć narzędziem-ujściem
`task.complete(wynik, podsumowanie, wyjścia, dowody)`**. Powiązanie z instancją procesu i aktywnością
nadaje serwer (wzór `generation.rs`), więc model nie może zakończyć cudzego zadania. Serwer waliduje
wyjścia względem schematu.

- Agent kończy bez wywołania ujścia (np. budżet się skończył) → wynik
  `wymaga_człowieka("agent nie zakończył zadania")`. Zasada: **brak jawnego wyniku nigdy nie jest
  sukcesem**.
- Agent zgłasza `błąd` z kodem → idzie przez zdarzenie brzegowe albo incydent jak każdy błąd.

### 4.3 Weryfikacja wyniku („zrobione” trzeba sprawdzić)

| Tryb | Jak | Dla czego |
|---|---|---|
| **zaufaj** | wynik agenta przyjmowany bez sprawdzenia | niskie ryzyko: streszczenie, szkic, klasyfikacja z późniejszą decyzją człowieka |
| **warunek** | wyrażenie CEL na wyjściach i danych systemu (np. `outputs.fixed && pipeline(outputs.mr).status == "success"`) | wszędzie, gdzie da się sprawdzić fakt |
| **test** | uruchomienie przepływu/testu weryfikującego (np. zestaw E2E na środowisku, skan) | poprawki kodu, zmiany konfiguracji |
| **krytyk** | drugi agent ocenia wynik względem definicji ukończenia (`critic_gate`) | zadania bez twardego testu (dokumentacja, analiza) |
| **człowiek** | zadanie użytkownika „Potwierdź wynik agenta” | kroki ryzykowne i nieodwracalne |

Tryby się łączą (np. „warunek + człowiek”). **Domyślny tryb dla nowego zadania agenta to „warunek”,
jeśli istnieje schemat wyniku, w przeciwnym razie „człowiek”.** Negatywna weryfikacja daje
`błąd(VERIFICATION_FAILED)` albo `wymaga_człowieka` — do wyboru w inspektorze.

### 4.4 Wywołanie przepływu, bloku, narzędzia, procesu

| Implementacja zadania usługi | Semantyka | Wynik |
|---|---|---|
| **blok** Flow Buildera | jednorazowe wykonanie pojedynczego bloku z wejściami z mapowania | `ukończone` + wyjścia bloku; wyjątek bloku → `błąd(BLOCK_ERROR)` |
| **przepływ** Flow Buildera | przebieg przepływu (krótki, jak dziś); wyjście bloku `output` = wyjścia | jak blok; przepływ może jawnie zwrócić wynik blokiem **„Wynik aktywności”** (nowy blok: ukończone / błąd / wymaga człowieka) |
| **narzędzie addonu** | wywołanie narzędzia z uprawnieniami pryncypała procesu | wynik narzędzia; odmowa uprawnień → `wymaga_człowieka` z kartą zgody |
| **wywołanie procesu** (call activity) | instancja procesu potomnego (trwała); rodzic czeka | wynik końca procesu potomnego (koniec zwykły = ukończone, koniec błędu = błąd z kodem) |

Nowy blok **„Wynik aktywności”** w palecie przepływów pozwala przepływowi obliczeniowemu jawnie
powiedzieć procesowi „nie udało się, kod X” zamiast rzucać wyjątek — to ten sam kontrakt po obu
stronach granicy.

## 5. Zmienne, dane, bezpieczeństwo

- **Zmienne procesu** = zmienne flow z HARNESS §3.12 (typy, CEL, io-mapping per aktywność). Zakresy:
  proces → podproces → aktywność. Przy imporcie z Camundy wyrażenia JUEL/FEEL są tłumaczone na CEL;
  czego nie da się przetłumaczyć, trafia na listę do ręcznej poprawy.
- **Limity:** rozmiar zmiennej, liczba zmiennych. Duże dane (pliki, raporty) zapisujemy jako odnośnik
  do CAS, a nie wartość.
- **Sekrety nigdy jako zmienne** — tylko odwołania do sejfu, rozwiązywane w bloku wykonującym.
- **Pryncypał procesu:** kto uruchomił i w czyim imieniu działają zadania usługi. Zadania agentów
  mają uprawnienia pryncypała, nie większe. Zadanie użytkownika zamyka osoba z przypisania (tor /
  grupa / wyrażenie), a zastępstwa pochodzą ze struktury organizacyjnej (`ORG_STRUCTURE_PLAN.md`).
- **Audyt:** każde przejście tokenu, decyzja człowieka, wynik agenta z modelem i każde ponowienie
  trafiają do historii instancji. Dziennik platformy jest łańcuchowany skrótami.

## 6. Monitor procesów

Odpowiednik Camunda Cockpit, dostępny w samym Flow Builderze:
- **instancje** z filtrami (definicja, wersja, stan, zmienna),
- **podgląd instancji na diagramie** — tokeny, przebyta ścieżka, gdzie czeka i na co,
- **historia i zmienne,**
- **incydenty** z przyciskiem „Ponów” i powiadomieniem właściciela procesu (nie tylko w monitorze),
- **migracja instancji** do nowej wersji definicji z mapą aktywności — ta sama reguła co mapa
  statusów w Projektach.

## 7. Konsekwencje dla planu Projektów

- `PROJECT_STUDIO_WORKFLOW_PLAN.md` §0 decyzja 3 i §3: workflow zadania = **proces BPMN w warstwie
  procesów**. Zadanie w Projektach = instancja procesu, a kolumna tablicy = aktywne zadanie
  użytkownika (albo grupa aktywności), więc przeciągnięcie karty zamyka zadanie użytkownika
  z wyborem przepływu.
- Bramka równoległa „Tester + Agent testujący + Agent security” to równoległe aktywności z kontraktem
  wyniku §4 (werdykty jako wyjścia), złączone bramką.
- SLA to **zdarzenie brzegowe czasowe** (nieprzerywające) na zadaniu użytkownika, liczone
  w kalendarzu roboczym.
- Zdarzenia git (MR otwarty/scalony) to **wiadomości** korelowane kluczem zadania.
- **Kolejność:** warstwa procesów (B1–B2) musi powstać przed fazą P2 Projektów (workflow). Do tego
  czasu Projekty działają na szablonie „Kanban prosty” (dzisiejsze 4 statusy).

## 8. Fazy

| Faza | Zakres |
|---|---|
| **B0 — prototyp decyzji** | 2–3 procesy (workflow zadania, PSIRT z zegarem CRA, zatwierdzenie) wykonane na prototypie warstwy procesów; pomiar i przegląd; potwierdzenie decyzji z §0 pkt 2 |
| **B1 — rdzeń** | definicje i wersje, instancje, tokeny, zadania użytkownika (trwałe), zadania usługi → joby do silnika flow i agentów, kontrakt wyniku, bramki XOR/AND, zdarzenia start/koniec, zmienne i io-mapping, historia |
| **B2 — czas i zdarzenia** | timery (także w kalendarzu roboczym), zdarzenia brzegowe (czasowe, błędu, eskalacji, wiadomości), wiadomości z korelacją (webhooki git, TentaBus), bramka inkluzywna i oparta na zdarzeniach, podproces, call activity, wielokrotne wykonanie |
| **B3 — edytor** | paleta BPMN + kategoria „Bloki TentaFlow” we Flow Builderze, dwa poziomy (proces → przepływ), inspektor z kontraktem wyniku, walidacja BPMN, import/eksport XML z DI i `camunda:`, symulacja |
| **B4 — monitor** | instancje, podgląd tokenów na diagramie, incydenty z ponowieniem i powiadomieniem, migracja instancji |
| **B5 — poziom 2** | podprocesy zdarzeniowe, kompensacja, zdarzenia warunkowe i linków, transakcje |

## 9. Otwarte decyzje

1. **Dwie warstwy (proces + flow) zamiast kompilacji BPMN do grafu bloków** — korekta HARNESS §6.
   Rekomendacja: tak (§0 pkt 2).
2. Domyślny tryb weryfikacji zadań agentów (§4.3) — propozycja: „warunek”, a bez schematu wyniku
   „człowiek”.
3. Czy warstwa procesów ma być od razu dostępna dla addonów (API do uruchamiania procesów
   i wysyłania wiadomości), czy na początek tylko dla aplikacji natywnych.
4. Czy przy imporcie procesów z NextApp (Camunda 7) utrzymujemy zgodność zmiennych `Static_*`,
   czy tylko czytamy diagram.

---

## 10. Stan realizacji (2026-10-08)

Źródłem prawdy jest kod: `tentaflow-core/src/processes/` (model, parser, runtime, repozytorium,
symulacja), migracje 183–199 w `db/migrations.rs`, protokół `tentaflow-protocol/src/processes.rs`
(`ProcessPayload`) i Flow Builder (`www/js/modules/flows-builder/`).

### 10.1 Zrealizowane

| Faza | Zakres | Commit |
|---|---|---|
| B1 — rdzeń | definicje i wersje, instancje, tokeny, zadania użytkownika, zadania usługi → joby silnika flow, kontrakt wyniku, bramki XOR/AND, start/koniec, zmienne i mapowanie, historia, edytor w Flow Builderze | `7b899ef5e` |
| B2 — czas | timery (start, zdarzenie pośrednie), zdarzenia brzegowe czasowe, kalendarz roboczy przypięty do wersji | `a455bd77d`, `3bf8a4a8f`, `27b399cc7` |
| B2 — zdarzenia | wiadomości z korelacją, błędy biznesowe, eskalacja do człowieka, sygnały, terminate | `d8c5cc135`, `82153668f`, `6e154dd25`, `6afd162f3` |
| B2 — struktura | podproces osadzony, call activity i koniec błędu, bramka inkluzywna, wielokrotne wykonanie (równoległe, sekwencyjne, pętla) | `42e4ba938`, `2607deacd`, `f739d3eb3`, `068e37617` |
| B2/B3 — zadania | skrypt CEL, zadanie ręczne, wysłanie i odbiór wiadomości, granice zadania wysłania, bramka oparta na zdarzeniach i oczekiwania w zakresach | `2eeeb9213`, `c16ae5922`, `cf1c9c144`, `6fa372f21`, `1f494159f` |
| B2/B3 — wynik aktywności, IO, symulacja | patrz 10.2 | `a7ca170bf` |

### 10.2 Co dostarcza ostatni krok

- **Blok „Wynik aktywności”** (`flow_engine/node_adapters/activity_result.rs`): przepływ jawnie
  zwraca `Completed` / `Error` / `NeedsHuman` / `Cancelled` z kodem, podsumowaniem, wyjściami
  i dowodami (§4.4). Walidacja grafu przy zapisie przepływu, szablon w palecie przepływów,
  a wykonawca uznaje przebieg z takim blokiem za błąd (`ACTIVITY_RESULT_NOT_PRODUCED`), gdy
  terminal się nie wykonał — brak wyniku nigdy nie jest sukcesem.
- **Trwałe wywołania usług** (migracja 198, `bpmn_service_invocations`): wywołanie zadania
  usługi ma fazy `prepared` → `may_have_executed` / `uncertain` → `observed` → `accepted` oraz
  ogrodzenie próby, więc wynik zaobserwowany po zamknięciu aktywności nie jest ponownie
  wykonywany ani cicho gubiony.
- **Wejścia i wyjścia aktywności (IO)** (migracje 197–199): skojarzenia danych wejściowych
  i wyjściowych na ośmiu rodzajach aktywności (użytkownika, skrypt, usługa, ręczna, wysłanie,
  odbiór, podproces, wywołanie), z trwałym świadkiem (`bpmn_activity_io_witnesses`) dla fazy
  przechwycenia wejść, zaakceptowanego wyniku i zastosowanych albo zablokowanych wyjść.
  Właściciel fazy to aktywność zwykła, porządkowa albo koordynator powtórzeń. Zablokowane
  mapowanie wyjścia zostawia aktywność oczekującą z incydentem, bez ponownego wykonania pracy.
- **Zdarzenia brzegowe na powtarzanej aktywności**: zewnętrzny timer albo wiadomość jest
  uzbrojony na koordynatorze i rozbrajany dopiero po zakończeniu całej grupy; migracja 196
  (`bpmn_repetition_accepted_source_instance`) wiąże przyjęte źródło porządkowe z instancją.
- **Dokumenty wieloprocesowe i modelowanie** (import i eksport BPMN XML, `processes/bpmn.rs`):
  wiele wykonywalnych procesów w jednym dokumencie z katalogiem startów, wywołanie procesu
  z tego samego dokumentu (`ProcessCallTarget::LocalBody`) obok opublikowanego, kolaboracja
  (uczestnicy, przepływy wiadomości), tory, obiekty i magazyny danych, adnotacje i skojarzenia
  zachowywane przy eksporcie, zdarzenia linku (throw/catch), przypięcie wybranego procesu
  i startu do instancji (migracja 197).
- **Symulacja procesu** (`processes/simulation.rs`, `SimulationRegistry`, okno w Flow
  Builderze): deterministyczne uruchomienie w prywatnej bazie SQLite z zegarem symulacji,
  stałymi identyfikatorami i śladem kroków. Obsługuje zadania skryptowe, użytkownika
  i ręczne, bramki oraz timer-catch; autoryzacja według ACL przechwyconej przy starcie.
  Limity: 16 uruchomień łącznie, 4 na organizację, 4 na właściciela, 16 MiB na uruchomienie,
  wygaszanie po 30 min bezczynności. Protokół:
  sześć par żądanie/odpowiedź `ProcessPayload::Simulation*` (dopisane na końcu enuma),
  obsługa w `dispatch/processes.rs`, kodek JS i klucze i18n w pięciu językach.

### 10.2a Poprawki po przeglądzie (2026-10-09)

- **Migracja 198 nie odmawia rozruchu z powodu kształtu historii zadań.** Historia zadań usługi,
  której poprzednia wersja nie mogła „udowodnić”, jest przekształcana w jawny stan zamiast
  przerywać migrację (po awarii nowy plik wykonywalny nigdy by nie wystartował). Zadanie
  `running` jest traktowane jak po rozruchowym `recover_jobs`: zadanie `error` z podbitym
  ogrodzeniem, a dla żywej aktywności także instancja w `incident` (zamknięta aktywność dostaje
  zadanie `cancelled` i rozwiązany incydent, bez zmiany statusu instancji), a wywołanie przechodzi w `uncertain` /
  `boundary_unknown` z **jednym** incydentem `EXTERNAL_OUTCOME_UNCERTAIN` (bez bezpośredniego
  ponowienia); przyczyna `INTERRUPTED` jest polem `reason` zdarzenia incydentu, tak jak w
  `fail_job`. Zadanie `completed` bez przyjętego zdarzenia źródłowego dostaje
  `SERVICE_HISTORY_UNPROVEN`, a próba bez znanego wyniku `EXTERNAL_OUTCOME_UNCERTAIN`.
  Zadanie `queued` bez prób (`attempt=0`, bez ogrodzenia i pracownika) staje się `prepared`.
  Zadanie `queued` z próbami staje się `prepared` tylko wtedy, gdy zdarzenie `job_retried`
  wymienia je jako NOWE zadanie (`new_job_id`) i po tym zdarzeniu (wyższe `seq`) nie ma zdarzenia
  `service_claimed` tego zadania — późniejsze pobranie oznacza ponowną wysyłkę, więc zadanie jest
  `uncertain`; rozwiązany incydent dowodzi jedynie, że jakaś
  wcześniejsza próba została obsłużona, a `old_job_id` oznacza, że zadanie zastąpiono ponowieniem
  (jest wyparte, więc nie jest wysyłane ponownie). Bez dowodu zadanie jest `uncertain` + otwarty
  `EXTERNAL_OUTCOME_UNCERTAIN` i zadanie `error`
  (żaden obecny zapis nie ponownie kolejkuje zadania z próbami, więc nigdy go nie
  wysyłamy ponownie z nowym identyfikatorem żądania). Zadanie ma co najwyżej jeden otwarty
  incydent: otwarty incydent starej wersji dla tego zadania (np. `SERVICE_FAILED`) jest
  rozwiązywany zdarzeniem `incident_resolved` (`superseded_by_upgrade`), a jego miejsce zajmuje
  incydent migracji. Incydent jest otwarty tylko dla żywej
  aktywności; dla zamkniętej zostaje rozwiązanym zapisem historii. Liczniki `retained_bytes` grup
  powtórzeń są przeliczane dla dotkniętych zadań. Limit zatrzymanych bajtów jest sprawdzany
  wyłącznie dla żywych instancji (status inny niż `completed`/`cancelled`/`error`); zamknięta
  instancja nigdy nie jest zatrzaskiwana, więc dodatkowa kopia przyjętego wyniku nie może
  zablokować startu węzła. Gdy żywa instancja przekracza limit (stan osiągalny dla starej wersji),
  migracja odtwarza to, co robi środowisko uruchomieniowe przy odmowie pojemności
  (`latch_repetition_capacity`): zatrzaskuje najbardziej obciążoną otwartą grupę (otwarty
  incydent `REPETITION_LIMIT`, zdarzenie `repetition_group_blocked` w fazie `capacity`,
  `terminal_capacity`) i anuluje całą instancję tą samą ścieżką co zamknięcie pojemnościowe
  w `cancel_instance_on` (powód `repetition_limit` z odwołaniem do zdarzenia blokady): tokeny,
  zadania, zlecenia Service, zadania użytkownika, timery, subskrypcje i wyścigi są zamykane, incydenty
  poza terminalnym incydentem grupy są rozwiązywane, a status instancji to `cancelled`. Dzięki temu
  żadne późniejsze przejście nie trafia na zatrzaśniętą grupę żywej instancji. To rozstrzygnięcie
  dotyczy wyłącznie instancji nadal żywej: status jest odczytywany ponownie na jego początku, więc
  instancja zamknięta wcześniejszym anulowaniem w tej samej migracji (np. wywołane dziecko
  anulowanego rodzica) nie dostaje incydentu ani zdarzenia. Gdy żywa instancja przekracza limit, a
  nie ma już otwartej grupy do zatrzaśnięcia, migracja nie zawodzi: zapisuje otwarty incydent
  `REPETITION_LIMIT` (`repetition_bytes`) i instancja zostaje w stanie `incident` do decyzji
  operatora (jedyny przypadek przekroczenia limitu bez anulowania).
  **Wywołane dziecko zamknięte anulowaniem pojemnościowym** (w środowisku uruchomieniowym i w
  migracji) powiadamia rodzica tak jak anulowanie przez właściciela: oczekujący wiersz
  `bpmn_calls` przechodzi w `cancelled`, a rodzic dostaje incydent `CALL_CHILD_CANCELLED` i status
  `incident` — chyba że rodzic jest anulowany w tej samej operacji (wiersz zamyka wtedy zamknięcie
  wywołań rodzica i incydent nie powstaje) albo jest już zamknięty (zmienia się tylko wiersz).
  `cancel_instance_on` i zamknięcie wywołań wymagają transakcji.
  **Migracja nadal odmawia (i cofa się w całości) wyłącznie przy uszkodzeniu lub manipulacji
  danymi**, czyli: niepusty `foreign_key_check` na wejściu lub na wyjściu oraz błąd
  `integrity_check`; niedokładny licznik bajtów grupy powtórzeń; zmieniony przypięty model
  (suma SHA-256) lub model, którego nie da się sparsować; zadanie bez przypiętej wersji; cykliczne
  lub zbyt głębokie pochodzenie zakresów; brak jedynego przypiętego węzła Service, węzeł innego
  rodzaju albo brak/niejednoznaczne przypięcie przepływu z poprawną sumą grafu; ujemne
  ogrodzenie; wieloznaczne lub sprzeczne z zapisanym wynikiem zdarzenia przyjętego wyniku.
- **Bramka oparta na zdarzeniach**: wejścia gałęzi Receive/Message/Signal są liczone przed
  uzbrojeniem czegokolwiek. Błąd skojarzenia (np. `1 / 0`, brakująca zmienna) parkuje token na
  gałęzi z incydentem `ACTIVITY_IO_INPUT_FAILED` (świadek `input_failed`), bez wyścigu,
  subskrypcji ani timera rodzeństwa; wcześniej przejście kończyło się błędem bez incydentu.
- **Budżet zdarzenia przechwycenia wejść** (384 KiB): `evaluate_activity_inputs` zlicza bajty
  kolejnych wejść i po przekroczeniu zgłasza błąd skojarzenia, czyli ten sam incydent
  `ACTIVITY_IO_INPUT_FAILED`; walidator odtworzeniowy wywołuje tę samą funkcję, więc wynik jest
  identyczny po obu stronach. Nie deduplikujemy wartości w zdarzeniu: kształt zdarzenia jest
  porównywany bajt w bajt przez walidator i czytany przez oś czasu, a świadek i tak zawiera
  pełny wektor wejść; zmiana wymagałaby jednoczesnej zmiany formatu zdarzenia, walidatora i UI.
- **Symulacja — zasoby.** Bezczynny przebieg wygasa po 30 min (`SIMULATION_IDLE_TTL`), limit
  4 przebiegów na organizację (obok 4 na właściciela i 16 na węzeł), a każdy `Start` najpierw
  odzyskuje przebiegi, których źródło zostało zarchiwizowane, zastąpione nową wersją lub którego
  właściciel jest nieaktywny. Przebieg w trakcie operacji nigdy nie jest wygaszany.
- **Symulacja — odpowiedzi.** Widok zwraca najnowsze zdarzenia (≤500, ≤4 MiB) i kroki śladu
  (≤200, ≤1 MiB) mieszczące się w 12 MiB ramki oraz liczniki `events_omitted` /
  `trace_steps_omitted` (pola dopisane z `#[serde(default)]`); okno symulacji pokazuje, ile
  starszych zdarzeń pominięto.
- **Symulacja — koszt `Start`.** `SimulationDatabase::open` odtwarzał pełną drabinkę migracji
  (197 kroków i kontrole spójności) przy każdym `Start`. Pomiar (profil testowy, jeden wątek,
  maszyna pod obciążeniem): 901 ms mediana z 5 prób (895–907 ms). Handlery synchroniczne
  (`#[handler]` bez `async`) są wykonywane bezpośrednio w `Box::pin(async move { f(req, ctx) })`,
  bez `spawn_blocking`, więc ten czas blokował wątek roboczy tokio. Schemat jest teraz budowany
  raz na proces i kopiowany API backup SQLite (cecha `backup` rusqlite): mediana 0,3 ms z 9 prób.
  Liczba w profilu release nie została zmierzona (nie budowano wariantu release); kopia stron
  pamięciowej bazy nie zależy od liczby kroków drabinki.
- **Symulacja — łączenie gałęzi w osobnych żądaniach.** Każde żądanie otwiera sklep od nowa z
  nowym alokatorem identyfikatorów, więc aktywacja rozwidlenia z wcześniejszego żądania była
  odrzucana jako „nie wydana przez alokator” przy dojściu do bramki łączącej. Plan może teraz
  powoływać się na aktywacje utrwalone w tokenach i pokwitowaniach przebiegu.
- **Symulacja — izolacja zapisu.** `ExecutionMode::Simulation` niesie dowód
  `PrivateSimulationMode`, który tworzy wyłącznie `simulation.rs`, a każdy punkt wejścia
  (`start_simulation_plan_on`, `apply_simulation_plan_on`, `fire_timer_on`) sprawdza, że
  transakcja działa na prywatnej bazie symulacji; transakcja produkcyjna jest odrzucana.

- **Runda 2 po przeglądzie.**
  - Anulowanie aktywacji z wierszem `uncertain` / `boundary_unknown` (bez krotki wysyłki) nie
    kończy się już błędem zapisu: `validate_closed_service_dispatches_on` pomija wywołania bez
    `committed_boundary`, tak jak migawka planera (ładuje tylko `committed_boundary`); ich incydent
    zamyka zwykłe anulowanie zadań aktywacji. Dotyczy zegara granicznego przerywającego i
    zakończenia terminującego w gałęzi siostrzanej.
  - Bramka zdarzeń: zapis sprawdza, że zgłoszona nieudana gałąź jest pierwszą nieudaną w kolejności
    krawędzi bramki (planer zatrzymuje się na pierwszej); wcześniejsza gałąź, która się powiedzie,
    jest wymagana.
  - `SimulationStartReservation::commit` porównuje także organizację przypiętego źródła.
  - `Start` symulacji: szablon schematu leży w `OnceLock` budowanym pod osobnym zamkiem
    budowy (równoległe pierwsze `Start` czekają na niego, nie na rejestr ani na zamek kopii);
    nieudana budowa nie jest zapamiętywana. Całą ciężką sekcję (`reclaim` z do 16 migawkami,
    pierwsza budowa szablonu, kopia) wykonuje `ui_channel::run_blocking`, czyli
    `block_in_place` na wielowątkowym środowisku tokio — handler jest synchroniczny, więc
    `spawn_blocking` nie ma tu zastosowania bez przebudowy sygnatury handlera. Nie zmierzono
    wpływu na opóźnienie ogona; sama kopia strony ma 0,3 ms.
  - `SimulationDatabase::open` jawnie włącza `PRAGMA foreign_keys=ON` (wbudowany SQLite ma to
    domyślnie włączone, więc zmiana jest zabezpieczeniem, nie poprawką zachowania).
  - Okno symulacji pokazuje łączną liczbę ukrytych zdarzeń (pominięte przez serwer plus
    odcięte po stronie klienta do 50 ostatnich) i `traceStepsOmitted`; klucz
    `bpmn.simulation_trace_steps_omitted` w pięciu językach.

**Ponów dla zablokowanego mapowania wyjścia — decyzja.** Nie wdrożone, bo nie da się tego zrobić
spójnie bez nowej migracji i nowej gałęzi walidatora:

- `output_blocked` jest świadkiem dowodowym: wyzwalacz CAS dopuszcza tylko przejście
  `result_accepted → output_applied | output_blocked`, a po zapisie incydent i wyjścia są
  niezmienne. Ponowna ewaluacja skojarzeń wyjścia na tych samych (przypiętych) danych daje ten
  sam błąd, więc „Ponów” miałoby sens wyłącznie jako ponowne liczenie względem bieżących zmiennych
  — to nowa semantyka, której plan nie definiuje.
- Wyświetlane `can_retry` pochodzi z `job_direct_retryable_on` (tylko zadanie usługi z fazą
  `proved_no_effect`), a `retry_job` przyjmuje wyłącznie `AcceptedInputRef::Retry` dla zadania.
- `SCRIPT_MAPPING_FAILED` z IO: świadek leży na tokenie aktywacji (`waiting`), a zaparkowany token
  to inny `token_id`. `uq_bpmn_io_activation` (instancja, zakres, `token_id`, właściciel fazy,
  porządek) nie blokuje zapisu świadka na zaparkowanym tokenie, ale ponowne użycie tokenu
  zerwałoby więź z przechwyconymi wejściami (walidator wymaga, by aktywacja i jej wejścia
  powstały w tym samym planie), a ponowne użycie tokenu aktywacji wymagałoby drugiego wiersza
  świadka o tym samym kluczu. Odpowiedź: nie, bez generacji w kluczu.
- B4 musi dodać: kolumnę generacji w kluczu `uq_bpmn_io_activation` i kolejny wiersz świadka po
  `output_blocked`, wpis `AcceptedInputRef::OutputRetry`, gałąź walidatora planu ponowienia,
  projekcję `can_retry` dla incydentów `ACTIVITY_IO_OUTPUT_FAILED` / `SCRIPT_MAPPING_FAILED` oraz
  zasadę, względem jakich zmiennych liczone jest ponowienie.

### 10.3 Co zostaje

- **B3**: kategoria „Bloki TentaFlow” i upuszczanie bloku/agenta jako zadania usługi,
  implementacje zadania usługi inne niż przepływ (blok, agent z ujściem `task.complete`,
  narzędzie addonu), tryby weryfikacji „test” i „krytyk” (dziś: warunek CEL lub człowiek),
  zakładka inspektora „Wynik i błędy”, import rozszerzeń `camunda:` (dziś kończy się jawną diagnostyką) wraz z tłumaczeniem
  wyrażeń JUEL/FEEL,
  symulacja poza profilem `script_user_manual` (usługi, wiadomości,
  sygnały, podprocesy, powtórzenia).
- **B4**: nakładka tokenów na diagramie instancji, ponowienia i powiadomienia o incydentach,
  migracja instancji do nowej wersji definicji.
- **B5**: podprocesy zdarzeniowe, kompensacja, zdarzenia warunkowe, bramka złożona,
  transakcje.
- Otwarte decyzje z §9 pozostają bez zmian.

### 10.4 Ryzyka integracyjne

- **Numeracja migracji.** `origin/main` zajmuje 178 (`cameras_depth_camera_offset`) i 179
  (`bus_schema_registry_widen_types`), dlatego migracje gałęzi po scaleniu mają numery 180–199
  (180–182 to struktura organizacyjna, 183–199 BPMN; przesunięcie o dwa względem wcześniejszych
  178–197). Testy przypinające numery (`run_ladder_up_to`, asercje `MAX(version)` w
  `db/migrations.rs`, `call_pin_tests.rs`) używają nowej numeracji. Baza, która zastosowała
  numery sprzed scalenia, nie może przejść na nową numerację bez ręcznej reconcylacji.
- **`SCHEMA_VERSION`.** Zmiany protokołu w tym kroku są addytywne: dopisane warianty
  `ProcessPayload::Simulation*` (tagi po NAZWIE), pole `ProcessModel.data_stores` z
  `#[serde(default)]` i serializacja `CallActivity` zgodna z dotychczasowym kształtem.
  Nie zmieniają zakodowanych bajtów istniejących wiadomości, więc nie wymagają podniesienia
  wersji; peer bez symulacji odrzuci tylko pojedyncze żądanie symulacji.
