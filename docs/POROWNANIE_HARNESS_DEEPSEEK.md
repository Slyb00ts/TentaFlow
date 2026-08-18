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

### 3.1 Narzędzia wołane sekwencyjnie
`tool_exec.rs` robi `for call in &calls` — jedno po drugim. To upraszcza (kolejność jest z
definicji zachowana, więc nie potrzeba maszynerii zatwierdzania w kolejności modelu), ale
**niezależne odczyty nie mogą jechać równolegle**. U nich: pula z limitem `maxParallelToolCalls`,
zatwierdzanie po ciągłych slotach, przeklasyfikowanie trybu tuż przed startem.

Nasza równoległość istnieje piętro wyżej — `agent_spawn` i sub-agenci. To inna jednostka
zrównoleglenia i nie zastępuje tamtej przy trzech `read_file` w jednej turze.

### 3.2 Brak budżetu czasu per narzędzie
Mamy sufit na `core.exec` (model podaje `timeout_secs`, my go ograniczamy), timeout `ask_user` i
dodany niedawno idle-timeout. **Nie mamy** odpowiednika `ToolDefinition.timeoutMs` egzekwowanego
przez jeden wrapper. Zawieszone narzędzie addonowe nie ma budżetu — to jest ta sama klasa problemu,
którą rozwiązaliśmy punktowo dla `core.exec`.

### 3.3 Brak ponowienia wewnątrz kroku
Ich krok jest pętlą: błąd żądania idzie przez waterfall `agent/request-error`, a `retry` **nie
zużywa kroku**. U nas błąd LLM przewraca iterację i zjada budżet pętli.

### 3.4 `max-tokens` nie jest lepkie
`llm.rs` liczy `truncated` per wywołanie, ale pętla nie niesie tego jako wyniku tury. U nich raz
uderzony sufit **nie może** zostać zdegradowany przez późniejszy zakończony normalnie krok.

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

1. **Budżet czasu per narzędzie** — najtańsze, usuwa całą klasę zawieszeń.
2. **Ponowienie wewnątrz kroku** — przestaje zjadać budżet pętli na błędach transportu.
3. **Lepkie `max-tokens`** — drobne, ale bez tego wynik tury potrafi kłamać.
4. **Model zdarzeń z `code_studio/events.rs` na poziom flow** — odblokowuje TTFT i czas narzędzia.
5. **Równoległe wywołania narzędzi** — dopiero gdy 1–4 są zrobione, bo wymaga zatwierdzania w
   kolejności modelu, żeby log pozostał deterministyczny.
6. **Rejestr składania promptu** — największa zmiana, osobny projekt.
