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
