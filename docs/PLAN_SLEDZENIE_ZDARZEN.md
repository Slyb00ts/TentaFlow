# Śledzenie zdarzeń: jedno miejsce, pochodzenie i aktor

Cel postawiony wprost: **dla każdego wywołania AI ma być widoczne SKĄD przyszło** (Code Studio,
zewnętrzne API, addon, projekt, zwykły czat, kamera, scheduler, mesh) **i KTO je wywołał**, a
przeglądanie ma być w jednym module. W Code Studio ten sam materiał ma być dostępny od razu przy
konkretnym przebiegu.

Wzorzec wizualny: Trajectory z DeepSeek Harness — przeanalizowany z kodu, opis w
[`ANALIZA_DEEPSEEK_HARNESS.md`](ANALIZA_DEEPSEEK_HARNESS.md) i
[`POROWNANIE_HARNESS_DEEPSEEK.md`](POROWNANIE_HARNESS_DEEPSEEK.md).

## 1. Stan zastany — mamy sześć powierzchni, żadna nie odpowiada na oba pytania

| Powierzchnia | Co trzyma | Czego brakuje |
|---|---|---|
| `audit_log` | akcja, `user_id`, `addon_id`, zasób, wynik, `severity` | brak pochodzenia; jeden wiersz na akcję, nie oś czasu przebiegu |
| `flow_executions` | jeden wiersz na przebieg flow | nie da się z niego policzyć TTFT ani czasu narzędzia |
| `agent_runs` + log przebiegu | stan i wynik przebiegu agenta | log jest tekstowy, nie zdarzeniowy |
| `alias_calls` | wywołania aliasów modeli | tylko warstwa modelu |
| `compliance_ai_events` | jedno wywołanie/sesja AI, wiązane z `audit_log` | świadomie BEZ treści promptów; to rejestr zgodności, nie narzędzie diagnostyczne |
| `code_studio` `events` | **pełna oś czasu sesji, `seq` ciągły, projekcje** | tylko Code Studio |

Wniosek: **wzorzec, którego potrzebujemy, już istnieje** — w `code_studio/events.rs`. Brakuje go na
poziomie przebiegu flow, i brakuje dwóch wymiarów w każdej powierzchni: `origin` i spójny `actor`.

## 2. Model docelowy

### 2.1 `run_events` — oś czasu przebiegu
Wzorowany 1:1 na `code_studio/events.rs`, bo ten model już się u nas obronił:
- `seq` alokowany jako `MAX(seq)+1` **wewnątrz transakcji insertu**, `UNIQUE(run_id, seq)` —
  drugi równoległy pisarz to głośny błąd, nie cicho poprzeplatany log,
- `idempotency_key` — powtórka jest no-opem, więc awaria między „efekt się wydarzył" a „zdarzenie
  zapisane" rozwiązuje się przez ponowny zapis,
- stany istniejące (`flow_executions`, `agent_runs`) zostają jako **projekcje**; przy rozjeździe
  wygrywa oś czasu.

**Zero nowej instrumentacji w adapterach.** Zapis karmi się `ProgressEvent`, które już emitujemy —
nowy `ProgressSink` obok istniejącego brokera. Ta zasada jest wprost zerżnięta z DeepSeeka: czasy
mają być RÓŻNICĄ dwóch zdarzeń, a nie polem, które ktoś musi pamiętać, żeby ustawić.

Brakujące zdarzenie: `FirstToken` (bez niego nie ma TTFT). To jedyny nowy punkt emisji.

### 2.1a Osobny plik SQLite — TAK, i to nie jest optymalizacja „na zapas"

`run_events` idzie do **własnego pliku** `<data>/events.db`, a nie do głównej bazy. Trzy powody,
z których pierwszy jest rozstrzygający:

1. **Główna baza ma JEDNO połączenie pisarza** i zapisy się w nim serializują (`db/mod.rs`:
   „`write()` bierze jedyne połączenie pisarza spod `Mutex`"). Log zdarzeń jest z definicji
   wysokoczęstotliwościowy — wrzucony tam, konkurowałby o ten sam zamek z zapisem ustawień, flow,
   agentów i audytu. To nie jest kwestia rozmiaru pliku, tylko opóźnienia całego systemu.
2. **Precedens jest u nas dosłowny.** `code_studio/workspace_db.rs` trzyma zdarzenia sesji w
   osobnym pliku i uzasadnia to jednym zdaniem, które pasuje tu bez zmiany ani słowa: *„deliberately
   not in the main database: they are runtime state of a single node, they are written constantly,
   and they must not travel through the Sync Ledger"*. Ta sama maszyneria (pula LRU, migracje przy
   otwarciu, `checkpoint_all` przy zamknięciu) już istnieje w `project_db.rs` i `workspace_db.rs`.
3. **Retencja robi się tania.** Kasowanie starych wierszy z dużej tabeli w głównej bazie zostawia
   bloat i wymusza VACUUM na pliku, od którego zależy cały system. Osobny plik można przycinać
   hurtem, a przy skrajnej objętości — rotować, nie ruszając niczego innego.

**Co to kosztuje, wprost:**
- **Brak JOIN-ów międzyplikowych.** Nazwy użytkowników, flow i agentów dociąga się po stronie
  aplikacji albo przez `ATTACH` w trybie read-only. Przeglądarka i tak czyta głównie jedną oś czasu.
- **Brak wspólnej transakcji** ze stanem w głównej bazie. Rozwiązanie jest już wymyślone: wzorzec
  `audit_outbox` z Code Studio — zdarzenie i jego **już zredagowana** kopia audytowa commitują się
  razem w pliku zdarzeń, a osobny krok przenosi kopię do `audit_log` w bazie głównej. Dzięki temu
  compliance nie może stracić wpisu, a diagnostyczna oś czasu może stracić ogon i nikomu to nie
  szkodzi. To rozróżnienie jest celowe: audyt jest zobowiązaniem, oś czasu jest narzędziem.

**Jeden plik na węzeł, nie na sesję.** Przeglądarka jest globalna i pyta w poprzek pochodzeń, więc
dzielenie per workspace (jak Code Studio) utrudniłoby główny przypadek użycia. Objętość
kontrolujemy retencją i `PRAGMA auto_vacuum=INCREMENTAL`, a rotacja miesięczna zostaje jako plan B,
jeśli produkcja pokaże, że retencja nie wystarcza.

### 2.2 Pochodzenie — `origin`
Stemplowane w `FlowRequestMeta` w punkcie wejścia; **nigdy nie pochodzi z treści modelu**.

| `origin` | Punkt wejścia |
|---|---|
| `code_studio` | sesja harnessu Code Studio |
| `api` | `/v1/*` (klucz API, aplikacja zewnętrzna) |
| `addon` | host-fn addona (`llm_generate`, `ingest_invoke`, engine-flow) |
| `project` | czat projektu, generowanie przypadków, ingest wiedzy |
| `chat` | zwykły czat dashboardu |
| `camera` | pipeline wizyjny |
| `scheduler` | zadanie z harmonogramu |
| `mesh` | przebieg zlecony przez inny węzeł |

### 2.3 Aktor
`actor_user_id` + `actor_kind` (`user` | `api_key` | `addon` | `system`). Dla przebiegów
zleconych przez mesh dochodzi węzeł zlecający — mamy już podpisane poświadczenie aktora
(`code_studio/assertion.rs`), więc źródło istnieje.

## 3. Czego NIE ruszamy

- **`audit_log` zostaje** i pozostaje rejestrem zgodności. Nowa oś czasu jest diagnostyczna;
  łączy się z audytem po `correlation_id`, ale go nie zastępuje.
- **`compliance_ai_events` zostaje bez treści promptów.** Retencja audytu AI nie może być krótsza
  niż 183 dni i to jest osobny reżim — nowy log ma własną, krótszą retencję.
- **`code_studio` `events` zostaje źródłem prawdy sesji.** Przeglądarka czyta z obu i skleja po
  `correlation_id`; nie migrujemy sesji do nowej tabeli.
- **Redakcja obowiązuje przed zapisem**, tak jak w `code_studio/redact.rs`. Nowy log przechodzi
  przez ten sam scrubber — inaczej stałby się tym wyciekiem, który ma wykrywać.

## 4. UI

### 4.1 Przeglądarka zdarzeń (nowy moduł)
Trzy warstwy, wzorowane na Trajectory:
1. **Oś czasu** — trzy tory: żądania modelu, wiadomości, narzędzia. Granice tur jako pionowe
   linie. Pasmo asystenta **rozcięte na TTFT i dekodowanie**. Przeciągnięcie zaznacza przedział i
   filtruje listę do rekordów aktywnych w tym przedziale.
2. **Rejestr** — indeks, zdarzenie, treść. Wirtualizowany, doładowuje starsze strony.
3. **Inspektor** — po zaznaczeniu: zużycie tokenów, czas, wejście, wyjście, chronometraż.

Filtry pierwszej klasy: **pochodzenie**, **aktor**, model, status, zakres czasu.

### 4.2 W Code Studio
Ta sama oś czasu jako zakładka przy sesji, zawężona do jej przebiegu — bez przechodzenia do
osobnego modułu. Materiał ten sam, inny zakres.

## 5. Etapy

1. **`origin` + `actor` w `FlowRequestMeta`** i stempel w każdym punkcie wejścia. Samo w sobie
   wartościowe: `flow_executions` od razu odpowiada „skąd i kto".
2. **`run_events`** — tabela, writer, `ProgressSink`. Plus zdarzenie `FirstToken`.
3. **Metryki jako zapytania** — czas narzędzia po `call_id`, TTFT, czas dekodowania.
4. **Przeglądarka** — oś czasu + rejestr + inspektor.
5. **Osadzenie w Code Studio.**
6. **Scalanie z osią sesji Code Studio** po `correlation_id`.

## 6. Ryzyka nazwane wprost

- **Objętość.** Log wszystkich przebiegów to dużo wierszy. Etap 2 musi przyjść z retencją i
  kompaktowaniem, inaczej pierwszy tydzień produkcji rozstrzygnie sprawę za nas.
- **Sync.** `run_events` jest runtime'owy, jak `flow_executions` i `audit_log` — **nie** wchodzi do
  Sync Ledger. Inaczej każdy węzeł replikowałby oś czasu każdego innego.
- **Redakcja.** Bez niej log jest wyciekiem. Wpięcie scrubbera to warunek Etapu 2, nie dodatek.
