# Nasz harness i Code Studio vs DeepSeek Harness

Porównanie mechanizm po mechanizmie, oba czytane z kodu. Odpowiedź na pytanie „czy mamy to samo
i czy mamy zabezpieczenia" brzmi: **w bezpieczeństwie jesteśmy wyraźnie dalej, w mechanice pętli
mamy cztery realne luki, a jedną rzecz z mojej wcześniejszej listy życzeń już mamy** — tylko po
innej stronie systemu, niż napisałem.

## 1. Korekta wcześniejszej analizy

W `RAG_UNIFICATION_PLAN.md` §9 napisałem, że nie mamy append-only logu z ciągłym `seq` i że to
fundament reszty. **To było nieścisłe.** `code_studio/events.rs` ma dokładnie ten model:

- `seq` alokowany jako `MAX(seq)+1` **wewnątrz transakcji insertu**,
- `UNIQUE(session_id, seq)` zamienia drugiego równoległego pisarza w **głośny błąd**, a nie w cicho
  poprzeplatany log,
- `idempotency_key` — powtórka jest no-opem zwracanym jako `duplicate`, więc awaria między „efekt
  się wydarzył" a „zdarzenie zapisane" rozwiązuje się przez ponowny zapis,
- stany (`sessions.status`, `session_runs.status`, dziennik operacji) są **projekcjami**; gdy
  rozjadą się z ogonem osi czasu, wygrywa oś czasu, a korekta sama jest zdarzeniem.

To jest ta sama zasada, co u DeepSeeka, momentami ostrzej postawiona (oni nie mają idempotency
key na zdarzeniu).

**Luka jest gdzie indziej: po stronie silnika flow.** `flow_executions` + `TraceStep` to model
„wiersz na przebieg". Z niego nie policzysz TTFT ani czasu pojedynczego narzędzia. Czyli §9 punkt 1
zwęża się do: *przenieść model z `code_studio/events.rs` na poziom przebiegu flow*, a nie
*zbudować go od zera*.

## 2. Gdzie jesteśmy dalej niż DeepSeek

| Obszar | My | DeepSeek Harness |
|---|---|---|
| Autoryzacja wywołań | `pep.rs` — JEDEN punkt (`authorize`) dla narzędzi, terminala i gita. Odmowa bije zgodę, obowiązkowe pytanie bije zapisaną zgodę, a `Allow` **nigdy nie istnieje bez nazwania profilu sandboxa** | Waterfall `tools/pre-execute` jako punkt zaczepienia; polityka należy do wdrożenia |
| Sieć | `egress/` — sandbox **nie ma trasy domyślnej**, jedynym wyjściem jest proxy, które **samo rozwiązuje DNS i przypina odpowiedź** na całą operację, więc nazwa zatwierdzona przez politykę i adres gniazda nie mogą się rozjechać | brak odpowiednika; `web_fetch` chodzi przez providera |
| Redakcja sekretów | `redact.rs` — scrubbing **przed zapisem**, nie przed odczytem; nic nieredagowanego nie dociera na dysk | telemetria **nie wozi żadnych reguł**; bez zamontowanego listenera rekordy wychodzą jak leci (ich własne „znane ograniczenie") |
| Trwałość audytu | `audit_outbox.rs` — **at-least-once**, świadomie: duplikat wpisu to uciążliwość, brakujący to porażka compliance | telemetria **at-most-once**; kursor znaczy „przekazane", nie „dostarczone" |
| Sekrety | `vault.rs` — szyfrowane kluczem węzła, **poza Sync Ledger**, wyjście tylko przez `SecretMaterial` (Debug drukuje `<redacted>`, Drop wyciera bufor) | `credentials` jako pakiet; brak porównywalnego opisu granicy |
| Tożsamość aktora w siatce | `assertion.rs` — podpisana Ed25519, `kid` + rotacja z **oknem nakładania**, żeby rotacja nie ucięła sesji w locie | jednoprocesowy; problem nie występuje |

Do tego mamy warstwy, których oni nie mają w ogóle: broker gita, przegląd patchy, zdalne proxy
przez mesh, indeks semantyczny, terminal.

## 3. Gdzie mamy realne luki

### 3.1 Narzędzia wołane sekwencyjnie **[NAPRAWIONE]**
`tool_exec.rs` robi `for call in &calls` — jedno po drugim. To upraszcza (kolejność jest z
definicji zachowana, więc nie potrzeba maszynerii zatwierdzania w kolejności modelu), ale
**niezależne odczyty nie mogą jechać równolegle**. U nich: pula z limitem `maxParallelToolCalls`,
zatwierdzanie po ciągłych slotach, przeklasyfikowanie trybu tuż przed startem.

Nasza równoległość istnieje piętro wyżej — `agent_spawn` i sub-agenci. To inna jednostka
zrównoleglenia i nie zastępuje tamtej przy trzech `read_file` w jednej turze.

**Zrobione.** Niezależne wywołania jadą równolegle w puli `max_parallel_tool_calls` (domyślnie 4),
a wyniki wracają w kolejności modelu, więc audyt i wiadomości `role=tool` wyglądają identycznie jak
przy wykonaniu sekwencyjnym. To, co wolno równolegle, jest **allowlistą**: same odczyty; każde
narzędzie mutujące, interaktywne i **każde narzędzie addonu** zostaje wyłączne.

### 3.2 Brak budżetu czasu per narzędzie **[NAPRAWIONE]**
Mamy sufit na `core.exec` (model podaje `timeout_secs`, my go ograniczamy), timeout `ask_user` i
dodany niedawno idle-timeout. **Nie mamy** odpowiednika `ToolDefinition.timeoutMs` egzekwowanego
przez jeden wrapper. Zawieszone narzędzie addonowe nie ma budżetu — to jest ta sama klasa problemu,
którą rozwiązaliśmy punktowo dla `core.exec`.

**Zrobione.** `tool_timeout_secs` (domyślnie 120 s) egzekwowany w JEDNYM miejscu — we wrapperze
wokół rozdzielacza, więc nowego narzędzia nie da się dodać z pominięciem limitu. Przekroczenie daje
**ustrukturyzowany wynik błędu**, nie awarię flow. Zwolnione są cztery narzędzia, które same trzymają
deadline (`ask_user`, `agent_wait`, `agent_spawn`, `exec`) — nałożenie drugiego tylko by go skróciło.
Granica mechanizmu: anulowanie działa przez drop future'a, więc zadanie `spawn_blocking`, które już
ruszyło, dobiegnie końca w tle i porzucony zostanie tylko jego wynik.

### 3.3 Brak ponowienia wewnątrz kroku **[NAPRAWIONE]**
Ich krok jest pętlą: błąd żądania idzie przez waterfall `agent/request-error`, a `retry` **nie
zużywa kroku**. U nas było gorzej, niż napisałem: błąd LLM propagował się z iteracji i **zabijał
cały przebieg**, nie tylko zjadał budżet.

**Zrobione.** Do `max_request_attempts` (domyślnie 3) prób z narastającym odstępem, wewnątrz kroku.
Klasyfikacja błędów jest heurystyką po tekście, bo `execute_chat` zwraca `anyhow` — i tak to
zapisałem, zamiast udawać precyzję. Kierunek pomyłki dobrany świadomie: **błąd nierozpoznany nie
jest powtarzany**. Ponowienia zatrzymuje anulowanie i deadline. Ścieżka strumieniowa świadomie
**nie** ma powtórek — po wysłaniu pierwszego tokenu w dół nie da się go cofnąć.

### 3.4 `max-tokens` nie jest lepkie **[NAPRAWIONE]**
`llm.rs` liczy `truncated` per wywołanie, ale pętla nie niesie tego jako wyniku tury. U nich raz
uderzony sufit **nie może** zostać zdegradowany przez późniejszy zakończony normalnie krok.

**Zrobione.** Znacznik `llm_truncated` zapisywany wyłącznie jako `true` i nigdy nie kasowany, a
pętla trzyma go we własnej fladze i stempluje wynik — więc grace-pass `final_pass`, który woła model
jeszcze raz, nie zamaskuje ucięcia z wcześniejszej iteracji.

### 3.5 Prompt składany raz, nie co krok
Nasz prompt systemowy siedzi w configu węzła `llm`. U nich składany jest **raz na krok** z wkładów
pluginów, więc zestaw narzędzi może się zmienić między krokami jednej tury. To jest fundament ich
wtyczkowości i najdroższa różnica do nadrobienia.

## 4. Wymaga sprawdzenia, nie twierdzę

- **Anulowanie w środku serii wywołań.** Oni dopisują syntetyczne wyniki błędu dla pominiętych
  wywołań, żeby replay logu pozostał poprawny. Nie zweryfikowałem, co nasz `tool_exec` zostawia w
  logu, gdy anulowanie wpadnie po `tool/call`, a przed wynikiem. Jeśli zostawia wywołanie bez
  wyniku, historia odtworzona z takiego logu ma dziurę w parze.

## 5. Kolejność nadrabiania (wg stosunku wartości do kosztu)

1. ~~Budżet czasu per narzędzie~~ — **zrobione**.
2. ~~Równoległe wywołania narzędzi~~ — **zrobione** (wcześniej, niż zakładała ta kolejność, bo
   zatwierdzanie w kolejności modelu wyszło za darmo: `join_all` zachowuje kolejność wejścia).
3. ~~Ponowienie wewnątrz kroku~~ — **zrobione**.
4. ~~Lepkie `max-tokens`~~ — **zrobione**.
5. **Model zdarzeń z `code_studio/events.rs` na poziom flow** — odblokowuje TTFT i czas narzędzia.
6. **Rejestr składania promptu** — największa zmiana, osobny projekt.

Poza listą, znalezione przy okazji: **zwolnienie `llm` z R4 było za szerokie** i przepuszczało dwie
krawędzie na ten sam port, gdzie `merge_inputs` po cichu wybrałby jedną. Naprawione — zwolnienie
działa per port, nie per węzeł.
